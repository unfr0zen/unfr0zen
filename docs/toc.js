// Marks the table of contents' link to the section in view: the last
// heading that has passed under the sticky header.
const links = new Map(
    [...document.querySelectorAll(".toc a")].map((a) => [a.getAttribute("href").slice(1), a])
);
const headings = [...links.keys()].map((id) => document.getElementById(id)).filter(Boolean);

function mark() {
    let current = null;
    for (const heading of headings) {
        if (heading.getBoundingClientRect().top <= 120) current = heading;
    }
    for (const [id, link] of links) {
        if (current && id === current.id) link.setAttribute("aria-current", "true");
        else link.removeAttribute("aria-current");
    }
}

addEventListener("scroll", mark, { passive: true });
addEventListener("resize", mark);
mark();
