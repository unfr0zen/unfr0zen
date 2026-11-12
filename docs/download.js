// Repository the release workflow publishes to.
const REPO = "unfr0zen/unfr0zen";
const RELEASES = `https://github.com/${REPO}/releases/latest`;

const LABELS = {
    "macos-universal": "↓ macOS (Universal)",
    "macos-arm64": "↓ macOS (Apple Silicon)",
    "macos-x86_64": "↓ macOS (Intel)",
    "windows-x86_64": "↓ Windows (x86_64)",
    "windows-arm64": "↓ Windows (arm64)",
    "linux-x86_64": "↓ Linux (x86_64)",
    "linux-arm64": "↓ Linux (arm64)",
    "linux-riscv64": "↓ Linux (riscv64)",
};

function detectOs() {
    const p = (navigator.userAgentData?.platform || navigator.platform || navigator.userAgent).toLowerCase();
    if (p.includes("mac")) return "macos";
    if (p.includes("win")) return "windows";
    return "linux";
}

function select(os) {
    for (const t of document.querySelectorAll(".tab")) {
        const on = t.dataset.os === os;
        t.classList.toggle("active", on);
        t.setAttribute("aria-selected", on);
    }
    for (const p of document.querySelectorAll(".panel")) {
        p.classList.toggle("active", p.id === `panel-${os}`);
    }
}

function fillButtons(assets) {
    for (const box of document.querySelectorAll("[data-files]")) {
        box.replaceChildren();
        for (const platform of box.dataset.files.split(" ")) {
            // The GUI build only; the CLI's assets are named unfr0zen-cli-*.
            const asset = assets.find((a) => a.name.startsWith(`unfr0zen-${platform}-`));
            const a = document.createElement("a");
            a.className = "btn btn-ice";
            a.textContent = LABELS[platform];
            a.href = asset ? asset.browser_download_url : RELEASES;
            box.append(a);
        }
    }
}

async function loadRelease() {
    fillButtons([]);
    try {
        const res = await fetch(`https://api.github.com/repos/${REPO}/releases/latest`);
        if (!res.ok) return;
        const release = await res.json();
        document.getElementById("version").textContent = `${release.tag_name} · ${new Date(release.published_at).toLocaleDateString()}`;
        fillButtons(release.assets);
    } catch {
        // Buttons already point at the releases page.
    }
}

function addCopyButtons() {
    for (const pre of document.querySelectorAll("pre")) {
        const b = document.createElement("button");
        b.className = "copy";
        b.textContent = "copy";
        b.addEventListener("click", async () => {
            await navigator.clipboard.writeText(pre.querySelector("code").textContent);
            b.textContent = "copied";
            setTimeout(() => (b.textContent = "copy"), 1500);
        });
        pre.append(b);
    }
}

for (const t of document.querySelectorAll(".tab")) {
    t.addEventListener("click", () => select(t.dataset.os));
}
select(detectOs());
addCopyButtons();
loadRelease();
