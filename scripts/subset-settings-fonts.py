"""Regenerate the OFL-licensed settings fonts with fontTools 4.68.0."""

from pathlib import Path

from fontTools import subset
from fontTools.ttLib import TTFont


root = Path(__file__).resolve().parents[1]
settings = root / "crates/usagi-settings"
fonts = settings / "ui/fonts"

# Preserve the complete KS X 1001 Hangul repertoire, Latin UI labels, and
# everything used in our static strings. Other characters use OS fallback.
characters = set(map(chr, range(0x20, 0x180)))
characters.update(map(chr, range(0x1100, 0x1200)))
characters.update(map(chr, range(0x3131, 0x318F)))
characters.update(
    "".join(bytes((lead, trail)).decode("euc_kr", errors="ignore")
            for lead in range(0xB0, 0xC9) for trail in range(0xA1, 0xFF))
)
for path in (
    *settings.glob("ui/*.slint"),
    *settings.glob("lang/**/*.po"),
    *settings.glob("src/*.rs"),
    *(root / "crates/usagi-core/src").rglob("*.rs"),
    root / "CHANGELOG.md",
):
    characters.update(path.read_text(encoding="utf-8"))

for weight in ("Medium", "SemiBold"):
    source = fonts / f"Pretendard-{weight}.otf"
    font = TTFont(source, recalcTimestamp=False)
    options = subset.Options()
    options.name_IDs = ["*"]
    options.name_languages = ["*"]
    subsetter = subset.Subsetter(options=options)
    subsetter.populate(unicodes=map(ord, characters))
    subsetter.subset(font)

    # Pretendard is the upstream Reserved Font Name. Keep attribution and
    # assign the modified faces their own family and PostScript names.
    for record in font["name"].names:
        if record.nameID in {1, 3, 4, 6, 16, 21, 25}:
            record.string = record.toUnicode().replace("Pretendard", "Usagi UI")
            if record.nameID in {3, 6, 25}:
                record.string = record.string.replace("Usagi UI", "UsagiUI")
    cff = font["CFF "].cff
    cff.fontNames = [f"UsagiUI-{weight}"]
    cff.topDictIndex[0].FamilyName = "Usagi UI"
    cff.topDictIndex[0].FullName = f"Usagi UI {weight}"
    font.save(fonts / f"UsagiUI-{weight}.otf")
