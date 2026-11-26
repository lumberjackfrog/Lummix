"""Plot Lummix package and audio measurements. Run with Python and matplotlib."""
import argparse
import json
import math
from pathlib import Path
from statistics import median

import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt
from matplotlib import patches


def measurements(app, audio):
    groups = [[], []]

    def app_row(label, old, new, scale, digits, unit):
        groups[0].append((label, old / scale, new / scale, digits, unit))

    def medians(group, key):
        return (app[group][side][key] for side in ("before", "after"))

    if "desktop" in app:
        desktop = app["desktop"]["summary"]
        app_row("Desktop ready to use",
                *(desktop[side]["desktopReadyMs"]["median"] for side in ("before", "after")),
                1000, 2, "s")
        trials = app["desktop"]["trials"]
        working_sets = [
            median(row["lummixWorkingSetBytes"] + row["webViewWorkingSetBytes"]
                   for row in trials if row["version"] == side)
            for side in ("before", "after")
        ]
        app_row("Desktop incl. WebView2", *working_sets, 1024 ** 2, 1, "MiB")
    app_row("Media service startup", *medians("media_medians", "ready_ms"), 1, 1, "ms")
    app_row("Read a gain fader", *medians("media_medians", "gains_median_ms"), 1, 2, "ms")
    app_row("Media service memory", *medians("media_medians", "private_mib"), 1, 1, "MiB")
    app_row("Install Lummix", *medians("setup_medians", "install_ms"), 1000, 2, "s")
    app_row("Verify driver files", *medians("setup_medians", "driver_verification_ms"), 1, 0, "ms")
    app_row("Installer size", *(app["installer_bytes"][s] for s in ("before", "after")),
            1_000_000, 2, "MB")
    for name, key in (("Microphone + music", "normal"), ("Voice ducking", "ducking"),
                      ("Peak limiting", "limiter")):
        case = audio["cases"][key]
        groups[1].append((name, case["before_us"], case["after_us"], 2, "µs"))
    return groups


def render(app, audio, target):
    groups = measurements(app, audio)
    changes = [100 * (1 - new / old) for rows in groups for _, old, new, _, _ in rows]
    lower = min(0, 25 * math.floor(min(changes) / 25))
    width = 12.4
    row_height = .43
    height = 4.42 + row_height * sum(map(len, groups))
    background, text, secondary, rule = "#171920", "#F4F3F6", "#B7BBC8", "#3F424D"
    mint, lilac, rose = "#79DFCA", "#C3B2FF", "#FFA597"
    plt.rcParams.update({"font.family": "DejaVu Sans", "svg.fonttype": "none"})
    fig = plt.figure(figsize=(width, height), facecolor=background)
    ax = fig.add_axes([0, 0, 1, 1])
    ax.set(xlim=(0, width), ylim=(height, 0))
    ax.axis("off")

    def label(x, y, value, size=12, color=text, weight="normal", align="left"):
        ax.text(x, y, value, va="center", ha=align, fontsize=size, color=color, weight=weight)

    left, right = .42, width - .42
    before_x, after_x, plot_l, plot_r = 4.48, 5.93, 6.40, 10.64

    def plot_x(value):
        return plot_l + (plot_r - plot_l) * (value - lower) / (100 - lower)

    label(left, .42, "Lummix", 15, mint, "bold")
    label(right, .42, "WINDOWS  /  x64", 9, secondary, "bold", "right")
    label(left, 1.02, "The Rust conversion, measured", 26, weight="bold")
    label(left, 1.49, "Times and memory are medians. Bars left of zero show an increase against the baseline.", 12, secondary)
    ax.plot([left, right], [1.82, 1.82], color=rule, lw=1)
    section_heads = ("Windows application", "Audio processing")
    descriptions = ("Hybrid launcher and services  →  standalone Rust",
                    "Previous capture and routing  →  Rust capture and routing")
    y = 2.12
    for index, rows in enumerate(groups):
        accent = (mint, lilac)[index]
        label(left, y, section_heads[index], 15, accent, "bold")
        label(left, y + .27, descriptions[index], 9.5, secondary)
        label(before_x, y, "Before", 10, secondary, "bold", "right")
        label(after_x, y, "After", 10, text, "bold", "right")
        label(plot_l, y, "Reduction", 10, secondary, "bold")
        label(right, y, "Saved", 10, secondary, "bold", "right")
        start = y + .63
        end = start + row_height * (len(rows) - 1)
        for tick in range(lower, 101, 25):
            x = plot_x(tick)
            ax.plot([x, x], [start - .16, end + .17], color=rule,
                    alpha=.98 if tick == 0 else .45, lw=.8)
            label(x, y + .30, f"{tick}%", 8, secondary, align="center")
        for i, (name, old, new, digits, unit) in enumerate(rows):
            cy = start + i * row_height
            change = 100 * (1 - new / old)
            ink = accent if change >= 0 else rose
            label(left, cy, name, 13)
            label(before_x, cy, f"{old:.{digits}f} {unit}", 11.5, secondary, align="right")
            label(after_x, cy, f"{new:.{digits}f} {unit}", 12, text, "bold", "right")
            x0, x1 = sorted((plot_x(0), plot_x(change)))
            ax.add_patch(patches.Rectangle((x0, cy - .078), x1 - x0, .156,
                                           facecolor=ink, edgecolor="none"))
            label(right, cy, f"{change:.0f}%", 13, ink, "bold", "right")
        y = end + .61
        ax.plot([left, right], [y - .24, y - .24], color=rule, lw=.8)
    foot_y = y + .035
    desktop = f"{app['desktop']['pairCount']} desktop, " if "desktop" in app else ""
    label(left, foot_y,
          f"Windows Server 2022  ·  {desktop}{len(app['setup_trials']['after'])} install and {app['pairs']} media pairs  ·  Desktop uses working sets. Media uses private bytes.",
          9, secondary)
    label(left, foot_y + .25,
          f"Ryzen 9 9950X3D  ·  {audio['block_frames']} frames at {audio['sample_rate'] // 1000} kHz  ·  "
          f"{audio['blocks_per_pass']:,} blocks per pass  ·  Physical headset latency remains unmeasured.",
          9, secondary)
    fig.savefig(target, dpi=175, facecolor=background, metadata={"Title": "Lummix Rust conversion benchmarks"})
    plt.close(fig)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("comparison", type=Path)
    parser.add_argument("audio", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    render(json.loads(args.comparison.read_bytes()), json.loads(args.audio.read_bytes()), args.output)

