"""Writes budget-2026.xlsx for the golden set's data cases (stdlib only; run from this
folder). Sheet "Budget": Category, Planned, Actual (euros); sheet "Notes": one text cell."""
import zipfile

ROWS = [("Clay and glaze", 4200, 4515.80), ("Kiln energy", 2600, 2310.25),
        ("Tools", 900, 1240), ("Marketing", 1500, 980.5), ("Rent", 9600, 9600)]
STR = ["Category", "Planned", "Actual"] + [r[0] for r in ROWS] + ["Numbers are euros, incl. VAT."]
NS = "http://schemas.openxmlformats.org"

def sheet(rows):
    return ('<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
            f'<worksheet xmlns="{NS}/spreadsheetml/2006/main"><sheetData>' + "".join(rows)
            + "</sheetData></worksheet>")

budget = ['<row r="1">' + "".join(f'<c r="{c}1" t="s"><v>{i}</v></c>' for i, c in enumerate("ABC")) + "</row>"]
for n, (cat, plan, act) in enumerate(ROWS, start=2):
    budget.append(f'<row r="{n}"><c r="A{n}" t="s"><v>{STR.index(cat)}</v></c>'
                  f'<c r="B{n}"><v>{plan}</v></c><c r="C{n}"><v>{act}</v></c></row>')
notes = [f'<row r="1"><c r="A1" t="s"><v>{len(STR) - 1}</v></c></row>']
files = {
    "[Content_Types].xml": ('<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        f'<Types xmlns="{NS}/package/2006/content-types">'
        '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>'
        '<Default Extension="xml" ContentType="application/xml"/>'
        '<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>'
        '<Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>'
        '<Override PartName="/xl/worksheets/sheet2.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>'
        '<Override PartName="/xl/sharedStrings.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml"/>'
        "</Types>"),
    "_rels/.rels": ('<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        f'<Relationships xmlns="{NS}/package/2006/relationships">'
        f'<Relationship Id="rId1" Type="{NS}/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>'
        "</Relationships>"),
    "xl/workbook.xml": ('<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        f'<workbook xmlns="{NS}/spreadsheetml/2006/main" xmlns:r="{NS}/officeDocument/2006/relationships">'
        '<sheets><sheet name="Budget" sheetId="1" r:id="rId1"/><sheet name="Notes" sheetId="2" r:id="rId2"/></sheets></workbook>'),
    "xl/_rels/workbook.xml.rels": ('<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        f'<Relationships xmlns="{NS}/package/2006/relationships">'
        f'<Relationship Id="rId1" Type="{NS}/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>'
        f'<Relationship Id="rId2" Type="{NS}/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet2.xml"/>'
        f'<Relationship Id="rId3" Type="{NS}/officeDocument/2006/relationships/sharedStrings" Target="sharedStrings.xml"/>'
        "</Relationships>"),
    "xl/sharedStrings.xml": ('<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        f'<sst xmlns="{NS}/spreadsheetml/2006/main" count="{len(STR)}" uniqueCount="{len(STR)}">'
        + "".join(f"<si><t>{s}</t></si>" for s in STR) + "</sst>"),
    "xl/worksheets/sheet1.xml": sheet(budget),
    "xl/worksheets/sheet2.xml": sheet(notes),
}
with zipfile.ZipFile("budget-2026.xlsx", "w", zipfile.ZIP_DEFLATED) as z:
    for name, data in files.items():
        z.writestr(zipfile.ZipInfo(name, date_time=(2026, 1, 1, 0, 0, 0)), data,
                   compress_type=zipfile.ZIP_DEFLATED)
