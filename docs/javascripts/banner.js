/* SPDX-License-Identifier: Apache-2.0
 * SPDX-FileCopyrightText: 2026 RustSys contributors
 */
/* The diagram is an illustration. A static poster is available at all times. */
(() => {
  const banner = document.querySelector(".rustsys-banner");
  if (!banner) return;
  const image = banner.querySelector("img[data-animation][data-poster]");
  const control = banner.querySelector(".rustsys-motion-control");
  const reduced = window.matchMedia("(prefers-reduced-motion: reduce)");
  if (!image || !control) return;
  let paused = reduced.matches;
  const apply = () => {
    // Remove <source> so an explicit user choice can override the media preference.
    banner.querySelectorAll("source").forEach(source => source.remove());
    image.src = paused ? image.dataset.poster : image.dataset.animation;
    control.textContent = paused ? "Play animation" : "Pause animation";
    control.setAttribute("aria-pressed", String(paused));
  };
  control.hidden = false;
  control.addEventListener("click", () => { paused = !paused; apply(); });
  reduced.addEventListener("change", event => { paused = event.matches; apply(); });
  apply();
})();
