"""Writes the XLSX fixtures for dn-data's tests (stdlib only; run from this folder).

orders.xlsx: sheet "Orders" (dates, numbers, booleans, one #DIV/0! cell) and an empty sheet.
orders-1904.xlsx: the same first sheet in the 1904 date system.
"""
import datetime
import zipfile

def serial(d, is1904):
    base = datetime.date(1904, 1, 1) if is1904 else datetime.date(1899, 12, 30)
    return (d - base).days

STRINGS = ["Date", "Amount", "Paid", "Ratio"]

def sheet_xml(is1904):
    rows = [
        (datetime.date(2026, 3, 1), 10.5, True, 0.5),
        (datetime.date(2026, 3, 15), 20.25, False, None),
        (datetime.date(2026, 4, 2), 30, True, 0.25),
    ]
    out = ['<row r="1">' + "".join(
        f'<c r="{c}1" t="s"><v>{i}</v></c>' for i, c in enumerate("ABCD")) + "</row>"]
    for n, (d, amount, paid, ratio) in enumerate(rows, start=2):
        cells = [
            f'<c r="A{n}" s="1"><v>{serial(d, is1904)}</v></c>',
            f'<c r="B{n}"><v>{amount}</v></c>',
            f'<c r="C{n}" t="b"><v>{1 if paid else 0}</v></c>',
            f'<c r="D{n}" t="e"><v>#DIV/0!</v></c>' if ratio is None
            else f'<c r="D{n}"><v>{ratio}</v></c>',
        ]
        out.append(f'<row r="{n}">' + "".join(cells) + "</row>")
    return ('<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
            '<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">'
            "<sheetData>" + "".join(out) + "</sheetData></worksheet>")

EMPTY = ('<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
         '<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">'
         "<sheetData/></worksheet>")

def write(path, is1904):
    ns = "http://schemas.openxmlformats.org"
    files = {
        "[Content_Types].xml": (
            '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
            f'<Types xmlns="{ns}/package/2006/content-types">'
            '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>'
            '<Default Extension="xml" ContentType="application/xml"/>'
            '<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>'
            '<Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>'
            '<Override PartName="/xl/worksheets/sheet2.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>'
            '<Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/>'
            '<Override PartName="/xl/sharedStrings.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml"/>'
            "</Types>"),
        "_rels/.rels": (
            '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
            f'<Relationships xmlns="{ns}/package/2006/relationships">'
            f'<Relationship Id="rId1" Type="{ns}/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>'
            "</Relationships>"),
        "xl/workbook.xml": (
            '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
            f'<workbook xmlns="{ns}/spreadsheetml/2006/main" xmlns:r="{ns}/officeDocument/2006/relationships">'
            + ('<workbookPr date1904="1"/>' if is1904 else "<workbookPr/>")
            + '<sheets><sheet name="Orders" sheetId="1" r:id="rId1"/><sheet name="Notes" sheetId="2" r:id="rId2"/></sheets>'
            "</workbook>"),
        "xl/_rels/workbook.xml.rels": (
            '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
            f'<Relationships xmlns="{ns}/package/2006/relationships">'
            f'<Relationship Id="rId1" Type="{ns}/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>'
            f'<Relationship Id="rId2" Type="{ns}/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet2.xml"/>'
            f'<Relationship Id="rId3" Type="{ns}/officeDocument/2006/relationships/styles" Target="styles.xml"/>'
            f'<Relationship Id="rId4" Type="{ns}/officeDocument/2006/relationships/sharedStrings" Target="sharedStrings.xml"/>'
            "</Relationships>"),
        "xl/styles.xml": (
            '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
            f'<styleSheet xmlns="{ns}/spreadsheetml/2006/main">'
            '<cellXfs count="2"><xf numFmtId="0"/><xf numFmtId="14" applyNumberFormat="1"/></cellXfs>'
            "</styleSheet>"),
        "xl/sharedStrings.xml": (
            '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
            f'<sst xmlns="{ns}/spreadsheetml/2006/main" count="{len(STRINGS)}" uniqueCount="{len(STRINGS)}">'
            + "".join(f"<si><t>{s}</t></si>" for s in STRINGS) + "</sst>"),
        "xl/worksheets/sheet1.xml": sheet_xml(is1904),
        "xl/worksheets/sheet2.xml": EMPTY,
    }
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
        for name, data in files.items():
            info = zipfile.ZipInfo(name, date_time=(2026, 1, 1, 0, 0, 0))
            z.writestr(info, data, compress_type=zipfile.ZIP_DEFLATED)

write("orders.xlsx", False)
write("orders-1904.xlsx", True)
