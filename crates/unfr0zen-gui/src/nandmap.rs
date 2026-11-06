//! A map of a whole-NAND image: its partitions, and which erase blocks hold
//! data, are erased, or are marked bad. Read on a background thread, since
//! an image is hundreds of megabytes.

use std::collections::{BTreeSet, HashMap};
use std::fs::File;
use std::io::{BufReader, Read};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;

use eframe::egui;
use unfr0zen_core::{diamond3, j49, Device};
use unfr0zen_host::nand;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Block {
    Erased,
    Data,
    Bad,
}

/// What a part of the NAND holds, for its colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Uboot,
    Env,
    SystemConfig,
    Root0,
    Root1,
    /// Everything else: kernels, user data, logs, oops logs.
    Other,
}

impl Kind {
    /// In the legend's order.
    const ALL: [Kind; 6] = [Kind::Uboot, Kind::Env, Kind::SystemConfig, Kind::Root0, Kind::Root1, Kind::Other];

    fn label(self) -> &'static str {
        match self {
            Kind::Uboot => "u-boot",
            Kind::Env => "env",
            Kind::SystemConfig => "system-config",
            Kind::Root0 => "root0",
            Kind::Root1 => "root1",
            Kind::Other => crate::tr!("nand.other"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Partition {
    pub name: String,
    pub kind: Kind,
}

/// A place on the grid: a block of the chip, or a block a UBI volume has
/// reserved and not written, which no block of the chip stands for yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Slot {
    block: Option<usize>,
    owner: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Map {
    pub blocks: Vec<Block>,
    pub partitions: Vec<Partition>,
    /// Each block's partition, an index into `partitions`.
    owners: Vec<Option<usize>>,
    /// The grid's places in order: the chip's blocks, or on gen 3 UBI's
    /// volumes in place of its partition, since wear levelling gives a
    /// volume's blocks no place on the chip.
    slots: Vec<Slot>,
    by_volume: bool,
    /// A block's data bytes, without the spare bytes a raw image holds too.
    pub block_size: usize,
    /// Whether the image can show a bad block. A gen 3 image has nothing to
    /// tell one by but the list beside it, which older backups lack.
    shows_bad: bool,
}

impl Map {
    pub fn bad(&self) -> usize {
        self.blocks.iter().filter(|b| **b == Block::Bad).count()
    }

    pub fn used(&self) -> usize {
        self.blocks.iter().filter(|b| **b == Block::Data).count()
    }

    /// What the map holds in a line.
    pub fn summary(&self) -> String {
        let used = crate::backups::size_label((self.used() * self.block_size) as u64);
        if self.shows_bad {
            crate::tr!("nand.summary_bad", used = used, count = self.bad())
        } else {
            crate::tr!("nand.summary", used = used)
        }
    }

    pub fn partition_of(&self, block: usize) -> Option<&Partition> {
        self.partitions.get((*self.owners.get(block)?)?)
    }

    fn kind_of(&self, block: usize) -> Kind {
        self.partition_of(block).map_or(Kind::Other, |p| p.kind)
    }

    /// The block a cell standing for `slots` shows, None when none holds data.
    /// A small part shows in any cell that holds a block of it; a large one by
    /// which has most.
    fn shown_block(&self, slots: &[Slot]) -> Option<usize> {
        let with = |kind: Kind| slots.iter().filter_map(|slot| slot.block)
            .filter(move |&b| self.blocks[b] == Block::Data && self.kind_of(b) == kind);
        [Kind::Env, Kind::SystemConfig, Kind::Uboot].into_iter().find_map(|kind| with(kind).next()).or_else(|| {
            [Kind::Root0, Kind::Root1, Kind::Other].into_iter()
                .filter_map(|kind| with(kind).next().map(|block| (with(kind).count(), block)))
                .reduce(|most, next| if next.0 > most.0 { next } else { most })
                .map(|(_, block)| block)
        })
    }
}

const MIB: usize = 1 << 20;

/// `(name, kind, blocks)` laid out in order, a later entry over an earlier
/// one, as a map's partitions and owners.
fn laid_out(layout: Vec<(&str, Kind, Range<usize>)>, total: usize) -> (Vec<Partition>, Vec<Option<usize>>) {
    let mut owners = vec![None; total];
    let mut partitions = Vec::new();
    for (name, kind, blocks) in layout {
        owners[blocks.start.min(total)..blocks.end.min(total)].fill(Some(partitions.len()));
        partitions.push(Partition { name: name.to_string(), kind });
    }
    (partitions, owners)
}

/// The gen 3 layout, as the image shows it: the blocks UBI's headers span,
/// the boot loader before them and the oops log after. With no UBI there
/// is nothing to tell the parts apart by.
fn diamond3_layout(ubi: Option<(usize, usize)>, total: usize) -> (Vec<Partition>, Vec<Option<usize>>) {
    let Some((first, last)) = ubi else {
        return laid_out(Vec::new(), total);
    };
    laid_out(vec![
        ("U-Boot", Kind::Uboot, 0..first),
        ("ubipart", Kind::Other, first..last + 1),
        ("oopsdata", Kind::Other, last + 1..total),
    ], total)
}

/// The gen 1 and 2 layout: the table in the image's own kernel. Without one
/// there is nothing to tell the parts apart by.
fn omap_layout(table: Option<Vec<j49::mtd::Partition>>, total: usize) -> (Vec<Partition>, Vec<Option<usize>>) {
    let block = j49::nand::BLOCK;
    let layout: Vec<_> = table.iter().flatten().map(|part| {
        let end = part.size.map_or(total, |size| (part.offset + size) / block);
        (part.name.as_str(), part_kind(&part.name), part.offset / block..end)
    }).collect();
    laid_out(layout, total)
}

/// UBI's own volume, which holds the table of the others.
const UBI_LAYOUT_VOLUME: u32 = 0x7fff_efff;
const UBI_TABLE_RECORD: usize = 172;
const UBI_TABLE_RECORDS: usize = 128;

fn be32(data: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_be_bytes(data.get(offset..offset + 4)?.try_into().ok()?))
}

/// What a block of the UBI partition holds, by its headers.
#[derive(Debug, PartialEq, Eq)]
enum UbiBlock {
    /// Not UBI's.
    Foreign,
    /// Formatted and holding nothing: only its erase counter is written.
    Free,
    /// A volume's logical block.
    Volume(u32, usize),
}

fn ubi_block(block: &[u8]) -> UbiBlock {
    if !block.starts_with(b"UBI#") {
        return UbiBlock::Foreign;
    }
    let header = be32(block, 16).map(|offset| offset as usize).and_then(|offset| block.get(offset..));
    let header = header.filter(|header| header.starts_with(b"UBI!"));
    match header.and_then(|header| Some((be32(header, 8)?, be32(header, 12)? as usize))) {
        Some((volume, leb)) => UbiBlock::Volume(volume, leb),
        None => UbiBlock::Free,
    }
}

/// The volumes in a block of UBI's layout volume: (volume, (name, blocks reserved)).
fn ubi_volume_table(block: &[u8]) -> Vec<(u32, (String, usize))> {
    let Some(table) = be32(block, 20).and_then(|offset| block.get(offset as usize..)) else {
        return Vec::new();
    };
    (0..UBI_TABLE_RECORDS).filter_map(|volume| {
        let record = table.get(volume * UBI_TABLE_RECORD..(volume + 1) * UBI_TABLE_RECORD)?;
        let len = u16::from_be_bytes([record[14], record[15]]) as usize;
        let name = record.get(16..16 + len).filter(|_| (1..128).contains(&len))?;
        let reserved = be32(record, 0)? as usize;
        Some((volume as u32, (String::from_utf8_lossy(name).into_owned(), reserved)))
    }).collect()
}

/// What a partition or a UBI volume holds, by its name.
fn part_kind(name: &str) -> Kind {
    match name {
        "ipl" | "spl0" | "spl1" => Kind::Uboot,
        "env0" | "env1" => Kind::Env,
        "system-config" => Kind::SystemConfig,
        "root0" => Kind::Root0,
        "root1" => Kind::Root1,
        _ => Kind::Other,
    }
}

/// Reads the image block by block.
pub fn read(path: &Path, device: Device) -> std::io::Result<Map> {
    let raw_block = match device {
        Device::Diamond3 => diamond3::nand::BLOCK as usize,
        _ => j49::nand::BLOCK_RAW,
    };
    let total = std::fs::metadata(path)?.len() as usize / raw_block;
    let listed = (device == Device::Diamond3).then(|| nand::read_bad_blocks(&nand::bad_blocks_path(path)))
        .transpose().unwrap_or_else(|e| {
            log::warn!("{e}");
            None
        }).flatten();
    scan(BufReader::with_capacity(raw_block * 8, File::open(path)?), total, device, listed.as_deref())
}

type BlockTest = fn(&[u8]) -> bool;

/// `listed`: the offsets of the blocks a gen 3 image's list names as bad.
fn scan(mut reader: impl Read, total: usize, device: Device, listed: Option<&[u32]>) -> std::io::Result<Map> {
    let (raw_block, block_size, bad, erased): (usize, usize, BlockTest, BlockTest) = match device {
        Device::Diamond3 => (diamond3::nand::BLOCK as usize, diamond3::nand::BLOCK as usize, |_| false, |block| block.iter().all(|&b| b == 0xff)),
        // A file system marks a block it erased in the spare bytes, which
        // leaves it holding nothing.
        _ => (j49::nand::BLOCK_RAW, j49::nand::BLOCK, j49::nand::factory_bad, |block| block.chunks(j49::nand::PAGE_RAW).all(j49::nand::erased)),
    };
    let mut table = j49::mtd::TableFinder::default();
    // Volumes take their blocks from the UBI partition in any order, and their
    // names may come later in it than their first blocks.
    let mut ubi: Option<(usize, usize)> = None;
    let mut written: Vec<(usize, u32, usize)> = Vec::new();
    let mut free: Vec<usize> = Vec::new();
    let mut table_of_volumes: HashMap<u32, (String, usize)> = HashMap::new();
    let mut buffer = vec![0u8; raw_block];
    let mut blocks = Vec::with_capacity(total);
    for index in 0..total {
        reader.read_exact(&mut buffer)?;
        let mut block = if bad(&buffer) {
            Block::Bad
        } else if erased(&buffer) {
            Block::Erased
        } else {
            Block::Data
        };
        if device != Device::Diamond3 {
            table.block(&buffer);
        } else {
            let found = ubi_block(&buffer);
            if found != UbiBlock::Foreign {
                ubi = Some((ubi.map_or(index, |(first, _)| first), index));
            }
            match found {
                UbiBlock::Foreign => {}
                UbiBlock::Free => {
                    block = Block::Erased;
                    free.push(index);
                }
                UbiBlock::Volume(volume, leb) => {
                    written.push((index, volume, leb));
                    if volume == UBI_LAYOUT_VOLUME {
                        table_of_volumes.extend(ubi_volume_table(&buffer));
                    }
                }
            }
        }
        if listed.is_some_and(|listed| listed.contains(&((index * block_size) as u32))) {
            block = Block::Bad;
        }
        blocks.push(block);
    }
    let (mut partitions, mut owners) = match device {
        Device::Diamond3 => diamond3_layout(ubi, total),
        _ => omap_layout(table.into_table(), total),
    };
    // A free block is no volume's until UBI maps it.
    for block in free {
        owners[block] = None;
    }
    let chip_order = |range: Range<usize>, owners: &[Option<usize>]| -> Vec<Slot> {
        range.map(|block| Slot { block: Some(block), owner: owners[block] }).collect()
    };
    let slots = match ubi {
        None => chip_order(0..total, &owners),
        Some((first, last)) => {
            let ids: BTreeSet<u32> = table_of_volumes.keys().copied().chain(written.iter().map(|w| w.1)).collect();
            // The partition's own blocks UBI does not know, kept on the grid.
            let foreign: Vec<usize> = (first..=last).filter(|&block| blocks[block] != Block::Erased && !written.iter().any(|w| w.0 == block)).collect();
            let mut slots = chip_order(0..first, &owners);
            let start = slots.len();
            for id in ids {
                let (name, reserved) = table_of_volumes.get(&id).cloned().unwrap_or_else(|| match id {
                    UBI_LAYOUT_VOLUME => (crate::tr!("nand.ubi_layout").to_string(), 0),
                    _ => (crate::tr!("nand.volume", id = id), 0),
                });
                let owner = partitions.len();
                partitions.push(Partition { kind: part_kind(&name), name });
                let mut places = vec![Slot { block: None, owner: Some(owner) }; reserved];
                for &(block, _, leb) in written.iter().filter(|w| w.1 == id) {
                    owners[block] = Some(owner);
                    let slot = Slot { block: Some(block), owner: Some(owner) };
                    match places.get_mut(leb) {
                        Some(place) if place.block.is_none() => *place = slot,
                        // Past its reservation, or an older copy of a logical
                        // block: every block keeps a place.
                        _ => places.push(slot),
                    }
                }
                slots.extend(places);
            }
            slots.extend(foreign.iter().map(|&block| Slot { block: Some(block), owner: owners[block] }));
            // What no volume reserves, to the partition's size.
            let used = slots.len() - start;
            slots.extend(std::iter::repeat_n(Slot { block: None, owner: None }, (last + 1 - first).saturating_sub(used)));
            slots.extend(chip_order(last + 1..total, &owners));
            slots
        }
    };
    let by_volume = ubi.is_some();
    Ok(Map { blocks, partitions, owners, slots, by_volume, block_size, shows_bad: device != Device::Diamond3 || listed.is_some() })
}

/// Maps read so far, and the ones being read.
#[derive(Default)]
pub struct Maps {
    done: Arc<Mutex<HashMap<PathBuf, Arc<Map>>>>,
    reading: Mutex<Vec<PathBuf>>,
}

impl Maps {
    /// The image's map, or None while it is read: the read starts here,
    /// once, and repaints when it lands.
    pub fn get(&self, path: &Path, device: Device, ctx: &egui::Context) -> Option<Arc<Map>> {
        if let Some(map) = self.done.lock().unwrap().get(path) {
            return Some(map.clone());
        }
        let mut reading = self.reading.lock().unwrap();
        if !reading.iter().any(|p| p == path) {
            reading.push(path.to_path_buf());
            let (done, path, ctx) = (self.done.clone(), path.to_path_buf(), ctx.clone());
            thread::spawn(move || {
                match read(&path, device) {
                    Ok(map) => { done.lock().unwrap().insert(path, Arc::new(map)); }
                    Err(e) => log::warn!("{}: {e}", path.display()),
                }
                ctx.request_repaint();
            });
        }
        None
    }

    /// Forgets an image's map, so a changed file is read again.
    pub fn forget(&self, path: &Path) {
        self.done.lock().unwrap().remove(path);
        self.reading.lock().unwrap().retain(|p| p != path);
    }
}

const COLUMNS: usize = 32;
/// A cell's side and the gap between cells, in points; both are rounded
/// to whole pixels on the display so every cell is the same size.
const CELL: f32 = 6.0;
const GAP: f32 = 1.5;

/// The cell side and gap in points at `pixels_per_point`.
fn geometry(pixels_per_point: f32) -> (f32, f32) {
    let side = (CELL * pixels_per_point).round().max(1.0) / pixels_per_point;
    let gap = (GAP * pixels_per_point).round().max(1.0) / pixels_per_point;
    (side, gap)
}

/// The map's width at `pixels_per_point`, for what holds it.
pub fn width(pixels_per_point: f32) -> f32 {
    let (side, gap) = geometry(pixels_per_point);
    COLUMNS as f32 * side + (COLUMNS as f32 - 1.0) * gap
}

/// The places a cell stands for.
fn cell_slots(map: &Map, cell: usize, per_cell: usize) -> &[Slot] {
    &map.slots[cell * per_cell..((cell + 1) * per_cell).min(map.slots.len())]
}

/// How bright an empty cell of a partition is, and the least a cell holding
/// data is; a cell full of data is at full brightness.
const EMPTY_SHADE: f32 = 0.25;
const USED_SHADE: f32 = 0.5;

/// A cell's colour: red when a block is bad, else its partition's, brighter
/// the more of its blocks hold data, and the plain erased colour when it is
/// empty and in no partition.
fn cell_color(map: &Map, group: &[Slot], colors: Colors) -> egui::Color32 {
    let holds = |block: Block| group.iter().filter(|slot| slot.block.is_some_and(|b| map.blocks[b] == block)).count();
    if holds(Block::Bad) > 0 {
        return colors.bad;
    }
    let share = holds(Block::Data) as f32 / group.len() as f32;
    match map.shown_block(group) {
        Some(block) => colors.of(map.kind_of(block)).gamma_multiply(USED_SHADE + (1.0 - USED_SHADE) * share),
        None => group.iter().find_map(|slot| map.partitions.get(slot.owner?))
            .map_or(colors.erased, |partition| colors.of(partition.kind).gamma_multiply(EMPTY_SHADE)),
    }
}

/// The map drawn as a grid of cells, each the places it stands for, shaded as
/// [`cell_color`] says. Hovering one tells where its block is on the chip.
pub fn draw(ui: &mut egui::Ui, map: &Map, colors: Colors) {
    let cells = map.slots.len().clamp(1, COLUMNS * 16);
    let per_cell = map.slots.len().div_ceil(cells);
    let cells = map.slots.len().div_ceil(per_cell);
    let rows = cells.div_ceil(COLUMNS);
    let ppp = ui.pixels_per_point();
    let (side, gap) = geometry(ppp);
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(width(ppp), rows as f32 * (side + gap) - gap), egui::Sense::hover());
    // The grid's origin on a pixel, so every cell edge is.
    let origin = egui::pos2((rect.min.x * ppp).round() / ppp, (rect.min.y * ppp).round() / ppp);
    let painter = ui.painter();
    for cell in 0..cells {
        let color = cell_color(map, cell_slots(map, cell, per_cell), colors);
        let (col, row) = (cell % COLUMNS, cell / COLUMNS);
        let min = origin + egui::vec2(col as f32 * (side + gap), row as f32 * (side + gap));
        painter.rect_filled(egui::Rect::from_min_size(min, egui::vec2(side, side)), 0.0, color);
    }
    if let Some(pos) = response.hover_pos() {
        let col = ((pos.x - origin.x) / (side + gap)) as usize;
        let row = ((pos.y - origin.y) / (side + gap)) as usize;
        let cell = row * COLUMNS + col;
        if cell < cells {
            let group = cell_slots(map, cell, per_cell);
            let text = match map.shown_block(group) {
                Some(block) => {
                    let name = map.partition_of(block).map_or("", |p| p.name.as_str());
                    crate::tr!("nand.block_hint", name = name, block = block, offset = format!("{:.1}", (block * map.block_size) as f64 / MIB as f64))
                }
                None => group.iter().find_map(|slot| map.partitions.get(slot.owner?))
                    .map_or(crate::tr!("nand.free").to_string(), |partition| crate::tr!("nand.empty_partition", name = partition.name)),
            };
            response.on_hover_text(text);
        }
    }
}

#[derive(Clone, Copy)]
pub struct Colors {
    pub uboot: egui::Color32,
    pub env: egui::Color32,
    pub system_config: egui::Color32,
    pub root0: egui::Color32,
    pub root1: egui::Color32,
    pub other: egui::Color32,
    pub erased: egui::Color32,
    pub bad: egui::Color32,
}

impl Colors {
    fn of(&self, kind: Kind) -> egui::Color32 {
        match kind {
            Kind::Uboot => self.uboot,
            Kind::Env => self.env,
            Kind::SystemConfig => self.system_config,
            Kind::Root0 => self.root0,
            Kind::Root1 => self.root1,
            Kind::Other => self.other,
        }
    }
}

const LEGEND_COLUMNS: usize = 3;
/// Leaves system-config, in the last column, inside the map on any display.
const LEGEND_COLUMN: f32 = 76.0;
const LEGEND_ROW: f32 = 13.0;

/// What the map's colours mean: in rows of three columns, each item in its
/// column so the rows line up.
pub fn legend(ui: &mut egui::Ui, map: &Map, colors: Colors) {
    let items: Vec<(&str, egui::Color32)> = Kind::ALL.into_iter().map(|kind| (kind.label(), colors.of(kind)))
        .chain([(crate::tr!("nand.erased"), colors.erased)])
        .chain(map.shows_bad.then_some((crate::tr!("nand.bad"), colors.bad))).collect();
    let rows = items.len().div_ceil(LEGEND_COLUMNS);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), (rows + usize::from(map.by_volume)) as f32 * LEGEND_ROW), egui::Sense::hover());
    if map.by_volume {
        ui.painter().text(rect.left_bottom() - egui::vec2(0.0, 0.5 * LEGEND_ROW), egui::Align2::LEFT_CENTER, crate::tr!("nand.volume_order_hint"),
                          egui::FontId::proportional(10.5), crate::DIM);
    }
    for (index, (name, color)) in items.iter().enumerate() {
        let (col, row) = (index % LEGEND_COLUMNS, index / LEGEND_COLUMNS);
        let left = rect.left_top() + egui::vec2(col as f32 * LEGEND_COLUMN, (row as f32 + 0.5) * LEGEND_ROW);
        let swatch = egui::Rect::from_center_size(left + egui::vec2(4.0, 0.0), egui::vec2(8.0, 8.0));
        ui.painter().rect_filled(swatch, 2.0, *color);
        ui.painter().text(left + egui::vec2(13.0, 0.0), egui::Align2::LEFT_CENTER, *name,
                          egui::FontId::proportional(10.5), crate::MUTED);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("unfr0zen-nandmap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn a_gen3_images_parts_are_found_from_where_its_ubi_blocks_are() {
        let block = diamond3::nand::BLOCK as usize;
        let path = temp("d3.bin");
        let mut image = vec![0xffu8; 12 * block];
        image[0] = 1;
        // UBI from the fourth block to the eighth, with a block it does not know.
        for index in 3..8 {
            image[index * block..(index + 1) * block].copy_from_slice(&ubi(None, &[]));
        }
        image[5 * block..6 * block].fill(0xff);
        image[5 * block] = 2;
        image[10 * block] = 3;
        std::fs::write(&path, &image).unwrap();
        let map = read(&path, Device::Diamond3).unwrap();
        assert_eq!(map.blocks.len(), 12);
        assert_eq!(map.block_size, block);
        assert_eq!((map.used(), map.bad()), (3, 0));
        assert_eq!(map.blocks[0], Block::Data);
        assert_eq!(map.blocks[1], Block::Erased);
        assert_eq!(map.blocks[3], Block::Erased, "UBI's and free");
        assert_eq!(map.partition_of(2).unwrap().name, "U-Boot");
        assert_eq!(map.partition_of(3), None, "free, so no volume's");
        assert_eq!(map.slots.len(), 12, "as many places as the chip has blocks");
        assert_eq!(map.partition_of(5).unwrap().name, "ubipart");
        assert_eq!(map.partition_of(7), None);
        assert_eq!(map.partition_of(8).unwrap().name, "oopsdata");
        assert_eq!(map.partition_of(11).unwrap().name, "oopsdata");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn a_gen3_image_with_no_ubi_names_nothing() {
        let block = diamond3::nand::BLOCK as usize;
        let mut image = vec![0xffu8; 4 * block];
        image[2 * block] = 1;
        let map = scan(std::io::Cursor::new(image), 4, Device::Diamond3, None).unwrap();
        assert_eq!(map.used(), 1);
        assert_eq!(map.partition_of(2), None);
    }

    #[test]
    fn a_gen2_image_finds_factory_bad_blocks_and_holds_no_names_without_a_kernel() {
        let path = temp("j49.bin");
        let mut image = vec![0xffu8; 2048 * j49::nand::BLOCK_RAW];
        // Block 3 marked bad in its first page's OOB; block 100 holds data.
        image[3 * j49::nand::BLOCK_RAW + j49::nand::PAGE] = 0;
        image[100 * j49::nand::BLOCK_RAW] = 0x42;
        // Block 200 is empty but for a file system's mark in its spare bytes.
        image[200 * j49::nand::BLOCK_RAW + j49::nand::PAGE + 8] = 0x85;
        std::fs::write(&path, &image).unwrap();
        let map = read(&path, Device::J49).unwrap();
        assert_eq!(map.blocks.len(), 2048);
        assert_eq!((map.used(), map.bad()), (1, 1));
        assert_eq!(map.block_size, j49::nand::BLOCK, "data bytes, without the spare ones");
        assert_eq!(map.blocks[3], Block::Bad);
        assert_eq!(map.blocks[200], Block::Erased);
        assert!(map.partitions.is_empty());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn only_a_gen2_image_counts_bad_blocks() {
        let gen3 = scan(std::io::Cursor::new(vec![0xffu8; diamond3::nand::BLOCK as usize]), 1, Device::Diamond3, None).unwrap();
        assert_eq!(gen3.summary(), format!("{} used", crate::backups::size_label(0)));
        let gen2 = scan(std::io::Cursor::new(vec![0xffu8; j49::nand::BLOCK_RAW]), 1, Device::J49, None).unwrap();
        assert_eq!(gen2.summary(), format!("{} used · 0 bad blocks", crate::backups::size_label(0)));
    }

    #[test]
    fn a_gen3_images_list_shows_its_blocks_as_bad_where_they_lie() {
        let block = diamond3::nand::BLOCK as usize;
        let mut image = vec![0xffu8; 12 * block];
        image[0] = 1;
        let mut put = |index: usize, data: Vec<u8>| image[index * block..(index + 1) * block].copy_from_slice(&data);
        put(2, leb(0, 0));
        // Uncorrectable and kept as read, in logical block 1 of env0.
        put(3, leb(0, 1));
        // Bad on the chip, so erased in the image, among UBI's blocks.
        put(5, ubi(Some(UBI_LAYOUT_VOLUME), &reserving(&[("env0", 2)])));
        // Uncorrectable where the volume's header would be: formatted and free otherwise.
        put(6, ubi(None, &[]));
        put(7, ubi(None, &[]));
        let listed = [3, 4, 6].map(|index| (index * block) as u32);
        let map = scan(std::io::Cursor::new(image), 12, Device::Diamond3, Some(&listed)).unwrap();
        assert_eq!(map.bad(), 3);
        assert_eq!(map.summary(), format!("{} used · 3 bad blocks", crate::backups::size_label(3 * block as u64)));
        assert_eq!(map.partition_of(3).unwrap().name, "env0", "kept in its volume");
        let places = |index: usize| map.slots.iter().filter(|slot| slot.block == Some(index)).count();
        assert_eq!([places(3), places(4), places(6)], [1, 1, 1], "each has a place to show red in");
        assert_eq!(map.slots.len(), 12, "as many places as the chip has blocks");
        assert_eq!(map.blocks[7], Block::Erased);
    }

    #[test]
    fn the_list_beside_a_gen3_image_reaches_its_map_and_a_gen2_image_ignores_one() {
        let block = diamond3::nand::BLOCK as usize;
        let path = temp("listed.bin");
        std::fs::write(&path, vec![0xffu8; 4 * block]).unwrap();
        assert!(!read(&path, Device::Diamond3).unwrap().shows_bad, "an older backup has no list");
        let list = nand::bad_blocks_path(&path);
        std::fs::write(&list, format!("{:08x}\n", 2 * block)).unwrap();
        let map = read(&path, Device::Diamond3).unwrap();
        assert!(map.shows_bad);
        assert_eq!(map.blocks[2], Block::Bad);
        assert_eq!(read(&path, Device::J49).unwrap().bad(), 0);
        std::fs::write(&list, "garbled\n").unwrap();
        assert!(!read(&path, Device::Diamond3).unwrap().shows_bad, "a broken list is no list");
        std::fs::remove_file(list).unwrap();
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn a_gen2_kernels_table_names_the_blocks_it_covers() {
        let block = j49::nand::BLOCK;
        let part = |name: &str, first: usize, blocks: usize| j49::mtd::Partition { name: name.into(), offset: first * block, size: Some(blocks * block) };
        let whole = j49::mtd::Partition { name: "nand0".into(), offset: 0, size: None };
        let table = vec![whole, part("ipl", 0, 2), part("spl0", 2, 2), part("env0", 4, 1), part("boot0", 5, 2),
                         part("root0", 7, 2), part("root1", 9, 2), part("system-config", 11, 2), part("data", 13, 2)];
        let (partitions, owners) = omap_layout(Some(table), 16);
        let map = Map { blocks: vec![Block::Data; 16], partitions, owners, slots: Vec::new(), by_volume: false, block_size: block, shows_bad: true };
        for (block, name, kind) in [(0, "ipl", Kind::Uboot), (3, "spl0", Kind::Uboot), (4, "env0", Kind::Env), (5, "boot0", Kind::Other),
                                    (8, "root0", Kind::Root0), (10, "root1", Kind::Root1), (12, "system-config", Kind::SystemConfig), (14, "data", Kind::Other)] {
            let partition = map.partition_of(block).unwrap();
            assert_eq!((partition.name.as_str(), partition.kind), (name, kind), "block {block}");
        }
        assert_eq!(map.partition_of(15).unwrap().name, "nand0", "what the others leave of the whole chip");
    }

    /// A block of UBI's: its erase counter's header, and a volume's when it
    /// holds one, then `data` where UBI's data starts.
    fn ubi(volume: Option<u32>, data: &[u8]) -> Vec<u8> {
        let mut block = vec![0xffu8; diamond3::nand::BLOCK as usize];
        block[..4].copy_from_slice(b"UBI#");
        block[16..20].copy_from_slice(&2048u32.to_be_bytes());
        block[20..24].copy_from_slice(&4096u32.to_be_bytes());
        if let Some(volume) = volume {
            block[2048..2052].copy_from_slice(b"UBI!");
            block[2056..2060].copy_from_slice(&volume.to_be_bytes());
            block[4096..4096 + data.len()].copy_from_slice(data);
        }
        block
    }

    /// UBI's volume table, naming each volume by its index.
    fn table(names: &[&str]) -> Vec<u8> {
        names.iter().flat_map(|name| {
            let mut record = vec![0u8; UBI_TABLE_RECORD];
            record[14..16].copy_from_slice(&(name.len() as u16).to_be_bytes());
            record[16..16 + name.len()].copy_from_slice(name.as_bytes());
            record
        }).collect()
    }

    #[test]
    fn a_gen3_images_blocks_are_named_by_their_ubi_volumes() {
        let block = diamond3::nand::BLOCK as usize;
        let names = table(&["env0", "system-config", "root0", "root1", "scratch"]);
        let mut image = vec![0xffu8; 40 * block];
        image[0] = 1;
        let mut put = |index: usize, data: Vec<u8>| image[index * block..(index + 1) * block].copy_from_slice(&data);
        // Each volume's block out of order, the table after the first of them.
        for (index, volume) in [(32, 2), (33, 0), (35, 1), (36, 3), (37, 4), (38, 2)] {
            put(index, ubi(Some(volume), &[]));
        }
        put(34, ubi(Some(UBI_LAYOUT_VOLUME), &names));
        put(39, ubi(None, &[]));
        let map = scan(std::io::Cursor::new(image), 40, Device::Diamond3, None).unwrap();
        for (block, name, kind) in [(0, "U-Boot", Kind::Uboot), (32, "root0", Kind::Root0), (33, "env0", Kind::Env),
                                    (35, "system-config", Kind::SystemConfig), (36, "root1", Kind::Root1),
                                    (37, "scratch", Kind::Other), (38, "root0", Kind::Root0)] {
            let partition = map.partition_of(block).unwrap();
            assert_eq!((partition.name.as_str(), partition.kind), (name, kind), "block {block}");
            assert_eq!(map.blocks[block], Block::Data);
        }
        assert_eq!(map.blocks[39], Block::Erased, "formatted and free holds nothing");
    }

    #[test]
    fn a_cell_is_brighter_the_fuller_it_is_and_red_when_bad() {
        let color = |value: u8| egui::Color32::from_rgb(value, value, value);
        let colors = Colors { uboot: color(1), env: color(2), system_config: color(4), root0: egui::Color32::WHITE,
                              root1: color(6), other: color(7), erased: color(8), bad: color(9) };
        let block = j49::nand::BLOCK;
        let part = j49::mtd::Partition { name: "root0".into(), offset: 0, size: Some(8 * block) };
        let (partitions, owners) = omap_layout(Some(vec![part]), 12);
        let mut blocks = vec![Block::Erased; 12];
        blocks[0..4].fill(Block::Data);
        blocks[4] = Block::Data;
        blocks[10] = Block::Bad;
        let slots = (0..12).map(|b| Slot { block: Some(b), owner: owners[b] }).collect();
        let map = Map { blocks, partitions, owners, slots, by_volume: false, block_size: block, shows_bad: true };
        let cell = |group: Range<usize>| cell_color(&map, &map.slots[group], colors);
        assert_eq!(cell(0..4), egui::Color32::WHITE, "full");
        assert_eq!(cell(4..8), egui::Color32::WHITE.gamma_multiply(USED_SHADE + (1.0 - USED_SHADE) * 0.25), "one block of four");
        assert_eq!(cell(5..8), egui::Color32::WHITE.gamma_multiply(EMPTY_SHADE), "empty, still its partition's");
        assert_eq!(cell(8..10), colors.erased, "empty and in no partition");
        assert_eq!(cell(8..12), colors.bad);
        assert!(cell(4..8).a() > cell(5..8).a() && cell(0..4).a() > cell(4..8).a());
    }

    /// A block of a volume's logical block.
    fn leb(volume: u32, leb: u32) -> Vec<u8> {
        let mut block = ubi(Some(volume), &[]);
        block[2060..2064].copy_from_slice(&leb.to_be_bytes());
        block
    }

    /// UBI's volume table: each volume's name and the blocks it reserves.
    fn reserving(volumes: &[(&str, u32)]) -> Vec<u8> {
        let mut records = table(&volumes.iter().map(|v| v.0).collect::<Vec<_>>());
        for (index, (_, reserved)) in volumes.iter().enumerate() {
            records[index * UBI_TABLE_RECORD..index * UBI_TABLE_RECORD + 4].copy_from_slice(&reserved.to_be_bytes());
        }
        records
    }

    #[test]
    fn a_gen3_grid_lays_each_volume_out_in_table_order_by_logical_block() {
        let block = diamond3::nand::BLOCK as usize;
        let mut image = vec![0xffu8; 20 * block];
        image[0] = 1;
        let mut put = |index: usize, data: Vec<u8>| image[index * block..(index + 1) * block].copy_from_slice(&data);
        // On the chip: root0's logical 2, env0's 0, root0's 0; the rest free.
        put(4, leb(1, 2));
        put(9, leb(0, 0));
        put(12, leb(1, 0));
        put(14, ubi(Some(UBI_LAYOUT_VOLUME), &reserving(&[("env0", 2), ("root0", 3)])));
        for index in 15..18 {
            put(index, ubi(None, &[]));
        }
        image[19 * block] = 2;
        let map = scan(std::io::Cursor::new(image), 20, Device::Diamond3, None).unwrap();
        assert!(map.by_volume);
        let names: Vec<(&str, Option<usize>)> = map.slots.iter().map(|slot| {
            (slot.owner.and_then(|o| map.partitions.get(o)).map_or("-", |p| p.name.as_str()), slot.block)
        }).collect();
        assert_eq!(names[..4], [("U-Boot", Some(0)), ("U-Boot", Some(1)), ("U-Boot", Some(2)), ("U-Boot", Some(3))]);
        assert_eq!(names[4..9], [("env0", Some(9)), ("env0", None), ("root0", Some(12)), ("root0", None), ("root0", Some(4))]);
        assert_eq!(names[9], ("UBI layout", Some(14)), "UBI's own after the volumes");
        assert_eq!(names.last(), Some(&("oopsdata", Some(19))));
        assert_eq!(names.len(), 20, "as many places as the chip has blocks");
        assert_eq!(map.partition_of(4).unwrap().name, "root0", "a block still knows its volume");
        let shown = map.slots.iter().filter(|slot| slot.block.is_some_and(|b| map.blocks[b] == Block::Data)).count();
        assert_eq!(shown, map.used(), "every block with data has a place");
    }

    #[test]
    fn a_second_copy_of_a_logical_block_keeps_a_place() {
        let block = diamond3::nand::BLOCK as usize;
        let mut image = vec![0xffu8; 6 * block];
        for (index, data) in [(1, leb(0, 0)), (2, leb(0, 0)), (3, ubi(Some(UBI_LAYOUT_VOLUME), &reserving(&[("env0", 1)])))] {
            image[index * block..(index + 1) * block].copy_from_slice(&data);
        }
        let map = scan(std::io::Cursor::new(image), 6, Device::Diamond3, None).unwrap();
        let held: Vec<usize> = map.slots.iter().filter_map(|slot| slot.block).filter(|&b| map.blocks[b] == Block::Data).collect();
        assert!(held.contains(&1) && held.contains(&2), "{held:?}");
    }

    #[test]
    fn a_gen2_grid_is_the_chip_in_order() {
        let map = scan(std::io::Cursor::new(vec![0xffu8; 3 * j49::nand::BLOCK_RAW]), 3, Device::J49, None).unwrap();
        assert!(!map.by_volume);
        assert_eq!(map.slots.iter().map(|slot| slot.block).collect::<Vec<_>>(), [Some(0), Some(1), Some(2)]);
    }

    #[test]
    fn a_cell_shows_a_small_volume_it_holds_and_otherwise_what_it_mostly_holds() {
        let block = diamond3::nand::BLOCK as usize;
        let names = table(&["env0", "root0", "root1"]);
        let mut image = vec![0xffu8; 56 * block];
        let mut put = |index: usize, volume: u32, data: &[u8]| image[index * block..(index + 1) * block].copy_from_slice(&ubi(Some(volume), data));
        for index in 32..35 { put(index, 2, &[]); }
        put(35, 1, &[]);
        for index in 36..39 { put(index, 2, &[]); }
        put(39, 0, &[]);
        put(40, UBI_LAYOUT_VOLUME, &names);
        let map = scan(std::io::Cursor::new(image), 56, Device::Diamond3, None).unwrap();
        let cell = |blocks: Range<usize>| blocks.map(|block| Slot { block: Some(block), owner: map.owners[block] }).collect::<Vec<_>>();
        assert_eq!(map.kind_of(map.shown_block(&cell(32..36)).unwrap()), Kind::Root1, "three of root1 to one of root0");
        assert_eq!(map.kind_of(map.shown_block(&cell(36..40)).unwrap()), Kind::Env, "env's one block among root1's");
        assert_eq!(map.shown_block(&cell(48..56)), None, "nothing but erased");
    }

    #[test]
    fn the_legend_rows_sit_close_and_its_columns_hold_their_longest_name() {
        let block = diamond3::nand::BLOCK as usize;
        let names = table(&["env0", "system-config", "root0", "root1", "other"]);
        let mut image = vec![0xffu8; 40 * block];
        image[0] = 1;
        for (index, volume) in [(32, 0), (33, 1), (34, 2), (35, 3), (36, 4)] {
            image[index * block..(index + 1) * block].copy_from_slice(&ubi(Some(volume), &[]));
        }
        image[37 * block..38 * block].copy_from_slice(&ubi(Some(UBI_LAYOUT_VOLUME), &names));
        let map = scan(std::io::Cursor::new(image), 40, Device::Diamond3, None).unwrap();
        let color = egui::Color32::WHITE;
        let colors = Colors { uboot: color, env: color, system_config: color, root0: color, root1: color, other: color, erased: color, bad: color };
        let output = egui::Context::default().run_ui(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width(1.0), 200.0))),
            ..Default::default()
        }, |ui| legend(ui, &map, colors));
        let text = |want: &str| output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(t) if t.galley.job.text == want => Some(t.galley.rect.translate(t.pos.to_vec2())),
            _ => None,
        }).unwrap_or_else(|| panic!("{want} is drawn"));
        for (upper, lower) in [("u-boot", "root0"), ("root0", "erased")] {
            let rows = text(lower).center().y - text(upper).center().y;
            assert_eq!(rows, LEGEND_ROW, "{upper} to {lower}");
        }
        assert_eq!(text("u-boot").left(), text("root0").left());
        assert!(text("system-config").right() <= width(1.0), "the last column holds it: {:?}", text("system-config"));
    }

    #[test]
    fn no_legend_name_runs_into_a_swatch_on_either_generation() {
        let color = egui::Color32::WHITE;
        let colors = Colors { uboot: color, env: color, system_config: color, root0: color, root1: color, other: color, erased: color, bad: color };
        let context = egui::Context::default();
        crate::install_fonts(&context);
        for (shows_bad, by_volume) in [(true, false), (false, true)] {
            let map = Map { blocks: Vec::new(), partitions: Vec::new(), owners: Vec::new(), slots: Vec::new(), by_volume, block_size: 1, shows_bad };
            let mut output = None;
            for _ in 0..2 {
                output = Some(context.run_ui(egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width(2.0), 200.0))),
                    ..Default::default()
                }, |ui| legend(ui, &map, colors)));
            }
            let output = output.unwrap();
            let swatches: Vec<egui::Rect> = output.shapes.iter().filter_map(|shape| match &shape.shape {
                egui::epaint::Shape::Rect(rect) if rect.rect.width() == 8.0 => Some(rect.rect),
                _ => None,
            }).collect();
            assert_eq!(swatches.len(), Kind::ALL.len() + 1 + usize::from(shows_bad));
            for shape in &output.shapes {
                if let egui::epaint::Shape::Text(text) = &shape.shape {
                    let name = text.galley.rect.translate(text.pos.to_vec2());
                    assert!(swatches.iter().all(|swatch| !name.intersects(*swatch)), "{} at {name:?}", text.galley.job.text);
                    assert!(name.right() <= width(2.0) + 1.0, "{} leaves the pane: {name:?}", text.galley.job.text);
                }
            }
        }
    }

    #[test]
    fn the_map_is_whole_pixels_wide_on_any_display() {
        assert_eq!(width(1.0), 32.0 * 6.0 + 31.0 * 2.0);
        assert_eq!(width(2.0), 32.0 * 6.0 + 31.0 * 1.5);
    }

    #[test]
    fn maps_are_read_once_and_forgotten_on_request() {
        let path = temp("small.bin");
        std::fs::write(&path, vec![0xffu8; 4 * j49::nand::BLOCK_RAW]).unwrap();
        let maps = Maps::default();
        let ctx = egui::Context::default();
        assert!(maps.get(&path, Device::J49, &ctx).is_none(), "read in the background");
        let mut map = None;
        for _ in 0..200 {
            map = maps.get(&path, Device::J49, &ctx);
            if map.is_some() { break }
            thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(map.unwrap().blocks.len(), 4);
        maps.forget(&path);
        assert!(maps.get(&path, Device::J49, &ctx).is_none());
        std::fs::remove_file(path).unwrap();
    }
}
