"""Check Help navigation with real Wayland pointer input on a disposable desktop.

Run inside an unlocked Omarchy session. Requires grim and wlrctl for this test,
not for the plugin. The caller supplies the observed top-bar button position.
"""

import argparse
import json
import re
import subprocess
import time
from pathlib import Path


def run(*command: str) -> str:
    return subprocess.check_output(command, text=True, timeout=10).strip()


def state() -> dict:
    return json.loads(run("omarchy-shell", "community.anolisa", "status"))


def wait_page(page: str, language: str) -> None:
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        current = state()
        if (
            current["opened"]
            and current["page"] == page
            and not current["busy"]
            and current["report"]["ui"]["language"] == language
        ):
            time.sleep(0.3)
            return
        time.sleep(0.1)
    raise AssertionError(f"The popup did not stay on {page}: opened={current['opened']}")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pointer", required=True)
    parser.add_argument("--bar-x", type=int, required=True)
    parser.add_argument("--bar-y", type=int, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    original = re.search(r"en-US|zh-CN", run("anolisa-omarchy", "language")).group()
    monitor = json.loads(run("hyprctl", "monitors", "-j"))[0]
    width = round(monitor["width"] / monitor["scale"])
    height = round(monitor["height"] / monitor["scale"])
    region_x = max(0, width - 600)
    region = f"{region_x},0 {width - region_x}x{min(height, 1200)}"
    results = []

    def move(x: int, y: int) -> None:
        for attempt in range(16):
            current = json.loads(run("hyprctl", "cursorpos", "-j"))
            dx, dy = x - current["x"], y - current["y"]
            if abs(dx) <= 2 and abs(dy) <= 2:
                return
            divisor = 1 if attempt == 0 else 2
            run(args.pointer, "pointer", "move", str(dx / divisor), str(dy / divisor))
        raise AssertionError("Pointer did not reach the test target")

    def click(x: int, y: int) -> None:
        move(x, y)
        run(args.pointer, "pointer", "click")

    def capture(name: str) -> tuple[tuple[int, int, int, int], bytes]:
        # PPM keeps image inspection dependency-free. Match the card border,
        # which is distinct from the wallpaper and the inner work card.
        data = subprocess.check_output(["grim", "-t", "ppm", "-g", region, "-"], timeout=10)
        header = re.match(rb"P6\s+(\d+)\s+(\d+)\s+255\s", data)
        if not header:
            raise AssertionError("Unexpected PPM screenshot header")
        image_width = int(header[1])
        pixels = data[header.end() :]
        matches = []
        offset = -1
        while (offset := pixels.find(bytes((58, 65, 69)), offset + 1)) != -1:
            if offset % 3 == 0:
                index = offset // 3
                matches.append((region_x + index % image_width, index // image_width))
        if len(matches) < 100:
            raise AssertionError("The popup border was not found in the desktop screenshot")
        bounds = (
            min(x for x, y in matches),
            min(y for x, y in matches),
            max(x for x, y in matches),
            max(y for x, y in matches),
        )
        card_region = (
            f"{bounds[0]},{bounds[1]} {bounds[2] - bounds[0] + 1}x{bounds[3] - bounds[1] + 1}"
        )
        run("grim", "-g", card_region, str(args.output / f"{name}.png"))
        return bounds, pixels

    def outside() -> None:
        click(100, min(800, height - 50))
        deadline = time.monotonic() + 3
        while state()["opened"] and time.monotonic() < deadline:
            time.sleep(0.1)
        if state()["opened"]:
            raise AssertionError("Clicking outside did not close the popup")

    try:
        for language in ("en-US", "zh-CN"):
            run("anolisa-omarchy", "language", language)
            for origin in ("home", "settings"):
                run("omarchy-shell", "community.anolisa", "close")
                click(args.bar_x, args.bar_y)
                wait_page("home", language)
                home, _ = capture(f"{language}-{origin}-opening")
                if origin == "settings":
                    click(home[2] - 82, home[1] + 40)
                    wait_page("settings", language)
                before, _ = capture(f"{language}-{origin}-before")
                if origin == "home":
                    # The compositor can present the language refresh after its
                    # IPC reply. Compare Back with the frame used for this click.
                    home = before
                click(before[2] - 40, before[3] - 20)
                wait_page("help", language)
                after, pixels = capture(f"{language}-{origin}-help")
                assert after == before, f"Help resized the popup: {before} -> {after}"
                move((after[0] + after[2]) // 2, (after[1] + after[3]) // 2)
                run(args.pointer, "pointer", "scroll", "500", "0")
                time.sleep(0.3)
                scrolled, scrolled_pixels = capture(f"{language}-{origin}-scrolled")
                assert scrolled == after, "Scrolling changed the popup boundary"
                if origin == "home":
                    assert pixels != scrolled_pixels, "Long Help content did not scroll"
                assert state()["opened"] and state()["page"] == "help"
                click(after[2] - 82, after[1] + 40)
                wait_page("home", language)
                returned, _ = capture(f"{language}-{origin}-returned")
                assert returned == home, f"Returning home changed its size: {home} -> {returned}"
                click(returned[2] - 40, returned[3] - 20)
                wait_page("help", language)
                outside()
                results.append(
                    {"language": language, "origin": origin, "bounds": after, "passed": True}
                )
                print(
                    f"PASS {language}: {origin} -> Help, stable boundary, scroll, Back, outside click",
                    flush=True,
                )
    finally:
        run("anolisa-omarchy", "language", original)
        run("omarchy-shell", "community.anolisa", "close")
        (args.output / "results.json").write_text(json.dumps(results, indent=2), encoding="utf-8")


if __name__ == "__main__":
    main()
