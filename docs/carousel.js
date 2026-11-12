// The hero's screenshots: the next one every 5 seconds, with arrows to step
// through them. They hold still under the mouse, on an arrow the keyboard has
// reached, and for anyone who asks their system for reduced motion.
const INTERVAL = 5000;

const root = document.querySelector(".shots");
const shots = [...root.querySelectorAll(".screenshot")];
const still = window.matchMedia("(prefers-reduced-motion: reduce)");
let current = 0;
let hovered = false;
let focused = false;
let timer;

function show(index) {
    shots[current].classList.remove("is-shown");
    shots[current].setAttribute("aria-hidden", "true");
    current = (index + shots.length) % shots.length;
    shots[current].classList.add("is-shown");
    shots[current].removeAttribute("aria-hidden");
}

// A full interval from now, so a shot stepped to by hand stays its whole time.
function schedule() {
    clearInterval(timer);
    if (!hovered && !focused && !still.matches) {
        timer = setInterval(() => show(current + 1), INTERVAL);
    }
}

for (const [selector, step] of [[".shot-prev", -1], [".shot-next", 1]]) {
    const button = root.querySelector(selector);
    button.hidden = false;
    button.addEventListener("click", () => {
        show(current + step);
        schedule();
    });
}

// A touch has no leaving to end its hover, so only a mouse counts.
root.addEventListener("pointerenter", (event) => {
    hovered = event.pointerType === "mouse";
    schedule();
});
root.addEventListener("pointerleave", () => {
    hovered = false;
    schedule();
});
// A click leaves focus on its arrow, which must not hold the rotation after
// the mouse has gone: only focus the keyboard moved counts.
root.addEventListener("focusin", (event) => {
    focused = event.target.matches(":focus-visible");
    schedule();
});
root.addEventListener("focusout", () => {
    focused = false;
    schedule();
});
still.addEventListener("change", schedule);

schedule();
