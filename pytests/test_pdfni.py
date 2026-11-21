from pathlib import Path

import pytest

import pdfni

DATA = Path(__file__).parent / "data"
SAMPLE = DATA / "sample.pdf"
ENCRYPTED = DATA / "encrypted.pdf"
PASSWORD = "pdfni-test"

TABLE_ROWS = [
    ["Item", "Qty", "Price"],
    ["Apple", "3", "120"],
    ["Orange", "5", "80"],
]
FOOTER = "pdfni fixture"
SECOND_PAGE = ["Second page marker.", FOOTER]


@pytest.fixture(scope="module")
def sample_bytes():
    return SAMPLE.read_bytes()


def cell_texts(table):
    return [[cell["text"] for cell in row] for row in table["data"]]


def block_texts(content):
    return [
        [block["text"] for block in page["blocks"] if block["type"] == "text"]
        for page in content["pages"]
    ]


# --- return shapes ---------------------------------------------------------


def test_tables_from_pdf(sample_bytes):
    doc = pdfni.extract_tables_from_pdf(sample_bytes)
    assert isinstance(doc, dict)
    assert len(doc["pages"]) == 2
    (table,) = doc["pages"][0]["tables"]
    assert (table["n_rows"], table["n_cols"]) == (3, 3)
    assert cell_texts(table) == TABLE_ROWS
    assert doc["pages"][1]["tables"] == []


def test_text_from_pdf(sample_bytes):
    doc = pdfni.extract_text_from_pdf(sample_bytes)
    assert isinstance(doc, dict)
    assert len(doc["pages"]) == 2
    page = doc["pages"][0]
    assert page["fonts"] and page["chars"]
    assert "".join(ch["text"] for ch in page["chars"]).startswith("Sample Report")


def test_text_items_from_pdf(sample_bytes):
    pages = pdfni.extract_text_items_from_pdf(sample_bytes)
    assert isinstance(pages, list) and len(pages) == 2
    assert pages[0][0]["str"] == "Sample Report"
    assert pages[1][0]["str"] == "Second page marker."


def test_text_lines_from_pdf(sample_bytes):
    pages = pdfni.extract_text_lines_from_pdf(sample_bytes)
    assert isinstance(pages, list) and len(pages) == 2
    first = pages[0][0]
    assert [word["text"] for word in first["words"]] == ["Sample", "Report"]


def test_document_from_pdf(sample_bytes):
    doc = pdfni.extract_document_from_pdf(sample_bytes)
    assert isinstance(doc, dict)
    page = doc["pages"][0]
    assert page["fonts"] and page["chars"] and page["blocks"]
    assert [block["type"] for block in page["blocks"]] == ["text", "text", "table", "text"]


def test_content_from_pdf(sample_bytes):
    content = pdfni.extract_content_from_pdf(sample_bytes)
    assert isinstance(content, dict)
    first, second = content["pages"]
    assert "chars" not in first
    heading, paragraph, table, footer = first["blocks"]
    assert (heading["kind"], heading["level"], heading["text"]) == (
        "heading",
        1,
        "Sample Report",
    )
    assert paragraph["kind"] == "paragraph"
    assert cell_texts(table) == TABLE_ROWS
    assert footer["text"] == FOOTER
    assert block_texts(content)[1] == SECOND_PAGE


def test_tables_from_parts():
    def edge(orientation, left, top, right, bottom):
        return {
            "orientation": orientation,
            "left": left,
            "top": top,
            "right": right,
            "bottom": bottom,
        }

    def glyph(ch, left, top):
        return {"ch": ch, "left": left, "right": left + 6, "top": top, "bottom": top + 10}

    xs, ys = [100, 200, 300], [100, 150, 200]
    edges = [edge("horizontal", xs[0], y, xs[-1], y) for y in ys]
    edges += [edge("vertical", x, ys[0], x, ys[-1]) for x in xs]
    glyphs = [
        glyph("A", 110, 120),
        glyph("B", 210, 120),
        glyph("C", 110, 170),
        glyph("D", 210, 170),
    ]
    page = {"width": 400, "height": 300, "glyphs": glyphs, "edges": edges}
    doc = pdfni.extract_tables({"pages": [page], "options": {"mode": "ruled"}})
    assert doc["source"] == "<parts>"
    (table,) = doc["pages"][0]["tables"]
    assert cell_texts(table) == [["A", "B"], ["C", "D"]]


def test_tables_from_parts_rejects_bad_input():
    with pytest.raises(ValueError, match="pages"):
        pdfni.extract_tables({"options": {}})


# --- source types ----------------------------------------------------------


@pytest.mark.parametrize(
    "make_source",
    [
        lambda data: bytearray(data),
        lambda data: memoryview(data),
        lambda data: memoryview(bytearray(data)),
        lambda data: str(SAMPLE),
        lambda data: SAMPLE,
    ],
    ids=["bytearray", "memoryview", "memoryview-mutable", "str-path", "pathlib"],
)
def test_source_types_match_bytes(sample_bytes, make_source):
    expected = pdfni.extract_content_from_pdf(sample_bytes)
    assert pdfni.extract_content_from_pdf(make_source(sample_bytes)) == expected


@pytest.mark.parametrize("source", [123, None, 1.5, ["a"]])
def test_source_wrong_type(source):
    with pytest.raises(TypeError, match="source must be"):
        pdfni.extract_content_from_pdf(source)


def test_source_missing_file(tmp_path):
    with pytest.raises(FileNotFoundError):
        pdfni.extract_content_from_pdf(tmp_path / "missing.pdf")


# --- keyword arguments and options -----------------------------------------


def test_pages_keyword(sample_bytes):
    content = pdfni.extract_content_from_pdf(sample_bytes, pages="2")
    assert block_texts(content) == [SECOND_PAGE]


def test_region_keyword(sample_bytes):
    content = pdfni.extract_content_from_pdf(sample_bytes, region=(0, 0, 600, 110))
    assert block_texts(content) == [["Sample Report"], ["Second page marker."]]


def test_mode_keyword(sample_bytes):
    ruled = pdfni.extract_tables_from_pdf(sample_bytes, mode="ruled")
    assert len(ruled["pages"][0]["tables"]) == 1
    methods = {
        table["extraction_method"]
        for page in pdfni.extract_tables_from_pdf(sample_bytes, mode="borderless")["pages"]
        for table in page["tables"]
    }
    assert "lattice" not in methods


def test_detect_header_footer_keyword(sample_bytes):
    def roles(content):
        return [
            [block.get("role") for block in page["blocks"] if block["type"] == "text"]
            for page in content["pages"]
        ]

    off = pdfni.extract_content_from_pdf(sample_bytes)
    on = pdfni.extract_content_from_pdf(sample_bytes, detect_header_footer=True)
    assert roles(off) == [[None, None, None], [None, None]]
    assert roles(on) == [[None, None, "footer"], [None, "footer"]]


def test_options_dict_matches_keywords(sample_bytes):
    by_keyword = pdfni.extract_content_from_pdf(
        sample_bytes, pages="1", region=(0, 0, 600, 110), mode="ruled"
    )
    by_options = pdfni.extract_content_from_pdf(
        sample_bytes,
        options={
            "pages": "1",
            "region": {"left": 0, "top": 0, "right": 600, "bottom": 110},
            "mode": "ruled",
        },
    )
    assert by_keyword == by_options


def test_keywords_override_options(sample_bytes):
    content = pdfni.extract_content_from_pdf(
        sample_bytes, pages="2", options={"pages": "1"}
    )
    assert block_texts(content) == [SECOND_PAGE]


def test_options_warnings_are_reported(sample_bytes):
    region = {"left": 0, "top": 0, "right": 600, "bottom": 900, "width": 1, "height": 1}
    content = pdfni.extract_content_from_pdf(sample_bytes, options={"region": region})
    assert len(content["warnings"]) == 1


def test_text_functions_take_pages_and_region(sample_bytes):
    assert len(pdfni.extract_text_from_pdf(sample_bytes, pages="2")["pages"]) == 1
    assert len(pdfni.extract_text_items_from_pdf(sample_bytes, pages="2")) == 1
    lines = pdfni.extract_text_lines_from_pdf(sample_bytes, region=(0, 0, 600, 110))
    assert [len(page) for page in lines] == [1, 1]


@pytest.mark.parametrize(
    "func",
    [
        pdfni.extract_text_from_pdf,
        pdfni.extract_text_items_from_pdf,
        pdfni.extract_text_lines_from_pdf,
    ],
)
def test_text_functions_reject_detection_keywords(sample_bytes, func):
    with pytest.raises(TypeError):
        func(sample_bytes, mode="ruled")
    with pytest.raises(TypeError):
        func(sample_bytes, detect_header_footer=True)


def test_arguments_after_source_are_keyword_only(sample_bytes):
    with pytest.raises(TypeError):
        pdfni.extract_content_from_pdf(sample_bytes, PASSWORD)


# --- errors ----------------------------------------------------------------


def test_exception_hierarchy():
    for exc in (
        pdfni.BadPasswordError,
        pdfni.SecurityRestrictedError,
        pdfni.InvalidFormatError,
    ):
        assert issubclass(exc, pdfni.PdfniError)
    assert issubclass(pdfni.PdfniError, Exception)
    assert not issubclass(pdfni.PdfniError, ValueError)


@pytest.mark.parametrize("password", [None, "wrong"])
def test_bad_password(password):
    with pytest.raises(pdfni.BadPasswordError):
        pdfni.extract_content_from_pdf(ENCRYPTED, password=password)


def test_correct_password_matches_plain(sample_bytes):
    assert pdfni.extract_content_from_pdf(
        ENCRYPTED, password=PASSWORD
    ) == pdfni.extract_content_from_pdf(sample_bytes)


@pytest.mark.parametrize("data", [b"", b"not a pdf"])
def test_not_a_pdf(data):
    with pytest.raises(pdfni.PdfniError):
        pdfni.extract_content_from_pdf(data)


@pytest.mark.parametrize(
    "kwargs",
    [
        {"pages": "0"},
        {"pages": "x"},
        {"mode": "unknown"},
        {"options": {"mode": "unknown"}},
        {"options": {"region": {"left": 0, "top": 0}}},
        {"region": (1, 2, 3)},
    ],
)
def test_invalid_arguments_raise_value_error(sample_bytes, kwargs):
    with pytest.raises(ValueError):
        pdfni.extract_content_from_pdf(sample_bytes, **kwargs)


def test_options_must_be_dict(sample_bytes):
    with pytest.raises(TypeError):
        pdfni.extract_content_from_pdf(sample_bytes, options=["mode"])
