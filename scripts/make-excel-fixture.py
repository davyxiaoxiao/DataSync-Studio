#!/usr/bin/env python3
"""Generate the .xlsx fixture used by the Excel import tests.

An .xlsx file is a zip of XML parts. Writing one by hand keeps the fixture
readable in review and avoids depending on a spreadsheet library just to
produce test data.

    python3 scripts/make-excel-fixture.py

The generated file exercises the cases the importer has to get right:
real numeric cells (which arrive as floats), booleans, an Excel date serial
with a date number format, empty cells, values that do not fit their column,
and a second worksheet.
"""

import pathlib
import zipfile
from xml.etree import ElementTree

ROOT = pathlib.Path(__file__).resolve().parent.parent
TARGET = ROOT / "src-tauri" / "tests" / "fixtures" / "import.xlsx"

MAIN = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"
REL = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"
PKG_REL = "http://schemas.openxmlformats.org/package/2006/relationships"

CONTENT_TYPES = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
<Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
<Override PartName="/xl/worksheets/sheet2.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
<Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/>
</Types>
"""

ROOT_RELS = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="{PKG_REL}">
<Relationship Id="rId1" Type="{REL}/officeDocument" Target="xl/workbook.xml"/>
</Relationships>
"""

WORKBOOK = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="{MAIN}" xmlns:r="{REL}">
<sheets>
<sheet name="数据" sheetId="1" r:id="rId1"/>
<sheet name="第二表" sheetId="2" r:id="rId2"/>
</sheets>
</workbook>
"""

WORKBOOK_RELS = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="{PKG_REL}">
<Relationship Id="rId1" Type="{REL}/worksheet" Target="worksheets/sheet1.xml"/>
<Relationship Id="rId2" Type="{REL}/worksheet" Target="worksheets/sheet2.xml"/>
<Relationship Id="rId3" Type="{REL}/styles" Target="styles.xml"/>
</Relationships>
"""

# numFmtId 14 is the built-in m/d/yyyy date format; a cell referencing it must
# come back as a date rather than as the serial number 45356.
STYLES = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<styleSheet xmlns="{MAIN}">
<fonts count="1"><font><sz val="11"/><name val="Calibri"/></font></fonts>
<fills count="1"><fill><patternFill patternType="none"/></fill></fills>
<borders count="1"><border/></borders>
<cellStyleXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellStyleXfs>
<cellXfs count="2">
<xf numFmtId="0" fontId="0" fillId="0" borderId="0" xfId="0"/>
<xf numFmtId="14" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/>
</cellXfs>
</styleSheet>
"""


def inline(ref: str, value: str) -> str:
    escaped = (
        value.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")
    )
    return f'<c r="{ref}" t="inlineStr"><is><t xml:space="preserve">{escaped}</t></is></c>'


def number(ref: str, value: str, style: int | None = None) -> str:
    attr = f' s="{style}"' if style is not None else ""
    return f'<c r="{ref}"{attr}><v>{value}</v></c>'


def boolean(ref: str, value: bool) -> str:
    return f'<c r="{ref}" t="b"><v>{1 if value else 0}</v></c>'


def blank(ref: str) -> str:
    return f'<c r="{ref}"/>'


def sheet(rows: list[list[str]]) -> str:
    # `"".join`, not f"{cells}": interpolating a list writes its repr, which
    # would put `['<c ...>', ...]` into the XML and escape every apostrophe.
    body = "".join(
        f'<row r="{index + 1}">{"".join(cells)}</row>' for index, cells in enumerate(rows)
    )
    return f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="{MAIN}"><sheetData>{body}</sheetData></worksheet>
"""


# --- sheet 1: the import target -------------------------------------------
# A trailing blank header cell so column naming has to fall back to 列N.
HEADER = [
    inline("A1", "id"),
    inline("B1", "name"),
    inline("C1", "amount"),
    inline("D1", "ratio"),
    inline("E1", "active"),
    inline("F1", "created_at"),
    inline("G1", "payload"),
    blank("H1"),
]

SHEET_1_ROWS = [
    HEADER,
    # Values a well-formed import must carry across, including a real numeric
    # cell, a boolean, and an Excel date serial.
    [
        number("A2", "1"),
        inline("B2", "苹果"),
        number("C2", "10.5"),
        number("D2", "0.25"),
        boolean("E2", True),
        number("F2", "45356", style=1),
        inline("G2", '{"a": 1}'),
        inline("H2", "第一行"),
    ],
    # An apostrophe in a string must survive quoting.
    [
        number("A3", "2"),
        inline("B3", "O'Brien"),
        number("C3", "20"),
        number("D3", "1.50"),
        boolean("E3", False),
        inline("F3", "2024/03/06 10:20:30"),
        inline("G3", '{"b": [1, 2]}'),
        blank("H3"),
    ],
    # Blank cells: the importer has to decide NULL versus "not allowed".
    [
        number("A4", "3"),
        inline("B4", "空值测试"),
        blank("C4"),
        blank("D4"),
        blank("E4"),
        blank("F4"),
        inline("G4", "null"),
        inline("H4", "第三行"),
    ],
    # Values that do not fit their column, so row-level reporting is exercised.
    [
        number("A5", "4"),
        inline("B5", "坏数值"),
        inline("C5", "abc"),
        inline("D5", "not-a-number"),
        inline("E5", "maybe"),
        inline("F5", "not-a-date"),
        inline("G5", "{oops"),
        inline("H5", "第四行"),
    ],
]

SHEET_2_ROWS = [
    [inline("A1", "code"), inline("B1", "label")],
    [inline("A2", "x"), inline("A2", "x")],
    [inline("A3", "y"), inline("B3", "第二张表")],
]


def main() -> None:
    parts = {
        "[Content_Types].xml": CONTENT_TYPES,
        "_rels/.rels": ROOT_RELS,
        "xl/workbook.xml": WORKBOOK,
        "xl/_rels/workbook.xml.rels": WORKBOOK_RELS,
        "xl/styles.xml": STYLES,
        "xl/worksheets/sheet1.xml": sheet(SHEET_1_ROWS),
        "xl/worksheets/sheet2.xml": sheet(SHEET_2_ROWS),
    }

    # A malformed part is worse than no fixture: a lenient reader may still
    # return values, and the tests then assert against corrupted data. Parsing
    # every part here makes that impossible to ship.
    for name, xml in parts.items():
        try:
            ElementTree.fromstring(xml)
        except ElementTree.ParseError as error:
            raise SystemExit(f"{name} 不是合法 XML: {error}") from error

    TARGET.parent.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(TARGET, "w", zipfile.ZIP_DEFLATED) as archive:
        for name, xml in parts.items():
            archive.writestr(name, xml)
    print(f"已生成 {TARGET.relative_to(ROOT)} ({TARGET.stat().st_size} 字节)")


if __name__ == "__main__":
    main()
