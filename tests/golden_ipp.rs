//! IPP request round-trip — build a Get-Printer-Attributes request, parse it back.

use std::io::Cursor;

use ipp::model::Operation;
use ipp::prelude::*;
use ipp::request::IppRequestResponse;

#[test]
fn build_get_printer_attributes_request() {
    let uri: Uri = "ipp://localhost:8631/ipp/print/test".parse().unwrap();
    let req = IppRequestResponse::new(
        IppVersion::v2_0(),
        Operation::GetPrinterAttributes,
        Some(uri),
    )
    .unwrap();
    let bytes = req.to_bytes();
    assert!(bytes.len() > 8);
}

#[test]
fn parse_get_printer_attributes_roundtrip() {
    let uri: Uri = "ipp://localhost:631/ipp/print/x".parse().unwrap();
    let req = IppRequestResponse::new(
        IppVersion::v2_0(),
        Operation::GetPrinterAttributes,
        Some(uri),
    )
    .unwrap();
    let bytes = req.to_bytes();

    let parsed = ipp::parser::IppParser::new(ipp::reader::IppReader::new(Cursor::new(bytes.to_vec())))
        .parse()
        .expect("parse");
    assert_eq!(
        parsed.header().operation_or_status,
        Operation::GetPrinterAttributes as u16
    );
}
