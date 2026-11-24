"""Rebuild the data for three of WeirdML v2's public tasks from their published prompts.

The official datasets are not public, so this is a reproduction: same shapes, sizes, file names
and label meanings as the prompts state, with our own choices where the prompts are silent
(how many of the 512 points lie on the shape, jitter, size range). Scores are comparable across
models run here, not with the WeirdML leaderboard.

    uv run --with numpy --with zstandard python bench/weirdml/gen_data.py OUT_DIR

Writes OUT_DIR/<task>/data/... (what the model's script sees) and OUT_DIR/<task>/labels.txt (the
test labels, kept outside the container). Chess games come from the open Lichess database
(January 2013, CC0), both players rated below 1300.
"""

import io
import re
import sys
import urllib.request
from pathlib import Path

import numpy as np

LICHESS = "https://database.lichess.org/standard/lichess_db_standard_rated_2013-01.pgn.zst"
SHAPES = ["circle", "square", "triangle", "pentagon", "star"]


def outline(shape: str) -> np.ndarray:
    """The shape's outline as a closed polygon of unit radius (a circle as a 256-gon)."""
    if shape == "circle":
        a = np.linspace(0, 2 * np.pi, 257)
        return np.stack([np.cos(a), np.sin(a)], 1)
    n = {"square": 4, "triangle": 3, "pentagon": 5, "star": 10}[shape]
    a = np.pi / 2 + np.arange(n + 1) * 2 * np.pi / n
    r = np.where(np.arange(n + 1) % 2 == 1, 0.4, 1.0) if shape == "star" else np.ones(n + 1)
    return np.stack([r * np.cos(a), r * np.sin(a)], 1)


def on_outline(poly: np.ndarray, n: int, rng: np.random.Generator) -> np.ndarray:
    """n points placed uniformly at random along the polygon's perimeter."""
    seg = np.diff(poly, axis=0)
    cum = np.concatenate([[0], np.cumsum(np.linalg.norm(seg, axis=1))])
    d = rng.uniform(0, cum[-1], n)
    i = np.searchsorted(cum, d, side="right") - 1
    return poly[i] + seg[i] * ((d - cum[i]) / (cum[i + 1] - cum[i]))[:, None]


def shapes(hard: bool, n: int, rng: np.random.Generator) -> tuple[np.ndarray, np.ndarray]:
    data, labels = np.empty((n, 512, 2), np.float32), rng.integers(0, 5, n)
    for k, label in enumerate(labels):
        m = int(rng.integers(64, 193)) if hard else 192  # points on the shape; the rest is noise
        size = rng.uniform(0.08, 0.3) if hard else 0.3
        pts = on_outline(outline(SHAPES[label]), m, rng)
        if hard:
            t = rng.uniform(0, 2 * np.pi)
            pts = pts @ np.array([[np.cos(t), -np.sin(t)], [np.sin(t), np.cos(t)]])
        centre = rng.uniform(size, 1 - size, 2) if hard else np.array([0.5, 0.5])
        pts = centre + size * pts + rng.normal(0, 0.004, pts.shape)
        allpts = np.concatenate([pts, rng.uniform(0, 1, (512 - m, 2))])
        data[k] = np.clip(rng.permutation(allpts), 0, 1)
    return data, labels


def write_shapes(out: Path, hard: bool, rng: np.random.Generator) -> None:
    (out / "data").mkdir(parents=True, exist_ok=True)
    train, train_labels = shapes(hard, 1000, rng)
    test, test_labels = shapes(hard, 1000, rng)
    np.save(out / "data/train_data.npy", train)
    np.save(out / "data/train_flags.npy", train_labels)
    np.save(out / "data/test_data.npy", test)
    np.savetxt(out / "labels.txt", test_labels, fmt="%d")


def write_chess(out: Path, rng: np.random.Generator) -> None:
    import zstandard

    (out / "data").mkdir(parents=True, exist_ok=True)
    raw = urllib.request.urlopen(LICHESS).read()
    text = zstandard.ZstdDecompressor().stream_reader(io.BytesIO(raw)).read().decode("utf-8", "replace")
    outcome = {"1/2-1/2": 0, "1-0": 1, "0-1": 2}
    games = []
    for block in text.split("\n\n[Event ")[1:]:
        head, _, moves = block.partition("\n\n")
        elo = [int(e) for e in re.findall(r'\[(?:White|Black)Elo "(\d+)"\]', head)]
        result = re.search(r'\[Result "([^"]+)"\]', head)
        if len(elo) != 2 or max(elo) >= 1300 or not result or result.group(1) not in outcome:
            continue
        moves = re.sub(r"\{[^}]*\}", "", moves)  # comments
        tokens = [t for t in moves.split() if t not in outcome and t != "*" and not t.endswith("...")]
        tokens = [re.sub(r"^(\d+)\.$", r"\1.", t) for t in tokens]
        plies = [t for t in tokens if not re.fullmatch(r"\d+\.", t)]
        if len(plies) < 10:
            continue
        if rng.random() < 0.5:
            plies = plies[:-1]
        line = " ".join(f"{i // 2 + 1}.{p}" if i % 2 == 0 else p for i, p in enumerate(plies))
        games.append((line, outcome[result.group(1)]))
    pick = rng.permutation(len(games))[:2000]
    train, test = [games[i] for i in pick[:1000]], [games[i] for i in pick[1000:]]
    (out / "data/train_games.txt").write_text("\n".join(g for g, _ in train) + "\n")
    np.save(out / "data/train_labels.npy", np.array([y for _, y in train]))
    (out / "data/test_games.txt").write_text("\n".join(g for g, _ in test) + "\n")
    np.savetxt(out / "labels.txt", np.array([y for _, y in test]), fmt="%d")
    print(f"chess: {len(games)} eligible games, label counts {np.bincount([y for _, y in test], minlength=3)}")


if __name__ == "__main__":
    root = Path(sys.argv[1])
    write_shapes(root / "shapes_easy", False, np.random.default_rng(1))
    write_shapes(root / "shapes_hard", True, np.random.default_rng(2))
    write_chess(root / "chess_winners", np.random.default_rng(3))
    print(f"wrote {root}")
