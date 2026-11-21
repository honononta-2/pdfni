import os
from typing import Any, Literal, TypeAlias

_Source: TypeAlias = bytes | bytearray | memoryview | str | os.PathLike[str]
_Region: TypeAlias = tuple[float, float, float, float]
_Mode: TypeAlias = Literal["auto", "ruled", "borderless"]

class PdfniError(Exception): ...
class BadPasswordError(PdfniError): ...
class SecurityRestrictedError(PdfniError): ...
class InvalidFormatError(PdfniError): ...

def extract_tables(parts: dict[str, Any]) -> dict[str, Any]: ...
def extract_tables_from_pdf(
    source: _Source,
    *,
    password: str | None = None,
    pages: str | None = None,
    region: _Region | None = None,
    mode: _Mode | None = None,
    detect_header_footer: bool | None = None,
    options: dict[str, Any] | None = None,
) -> dict[str, Any]: ...
def extract_text_from_pdf(
    source: _Source,
    *,
    password: str | None = None,
    pages: str | None = None,
    region: _Region | None = None,
    options: dict[str, Any] | None = None,
) -> dict[str, Any]: ...
def extract_text_items_from_pdf(
    source: _Source,
    *,
    password: str | None = None,
    pages: str | None = None,
    region: _Region | None = None,
    options: dict[str, Any] | None = None,
) -> list[list[dict[str, Any]]]: ...
def extract_text_lines_from_pdf(
    source: _Source,
    *,
    password: str | None = None,
    pages: str | None = None,
    region: _Region | None = None,
    options: dict[str, Any] | None = None,
) -> list[list[dict[str, Any]]]: ...
def extract_document_from_pdf(
    source: _Source,
    *,
    password: str | None = None,
    pages: str | None = None,
    region: _Region | None = None,
    mode: _Mode | None = None,
    detect_header_footer: bool | None = None,
    options: dict[str, Any] | None = None,
) -> dict[str, Any]: ...
def extract_content_from_pdf(
    source: _Source,
    *,
    password: str | None = None,
    pages: str | None = None,
    region: _Region | None = None,
    mode: _Mode | None = None,
    detect_header_footer: bool | None = None,
    options: dict[str, Any] | None = None,
) -> dict[str, Any]: ...
