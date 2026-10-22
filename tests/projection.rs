use git_monitor::{
    diff,
    document::ByteRange,
    projection::{self, ByteSpan, Side},
};
use proptest::prelude::*;
proptest! {#![proptest_config(ProptestConfig::with_cases(64))]
#[test]fn display_projection_preserves_selected_lines(old in "[a-zα🦀 \\r\\n]{0,200}",new in "[a-zβ🦋 \\r\\n]{0,200}"){
 let file=diff::build("x.txt",old.as_bytes(),new.as_bytes(),20,80,None,&Default::default(),&Default::default());file.validate().unwrap();let document=file.document.unwrap();for row in projection::rows(&document.hunks,old.as_bytes(),new.as_bytes()).unwrap(){for(number,spans,bytes,start)in[(row.old,row.left,old.as_bytes(),20),(row.new,row.right,new.as_bytes(),80)]{if let Some(number)=number{let range=projection::LineIndex::new(bytes).line(bytes,number,start).unwrap();prop_assert_eq!(spans.iter().map(|s|s.text.as_str()).collect::<String>(),String::from_utf8_lossy(&bytes[range.start..range.end]));}}}
}}
#[test]
fn rejects_neighbor_spans_wrong_lines_and_encoding_labels() {
    let file = diff::build(
        "x.c",
        b"x=1;\n",
        b"x=2;\n",
        30,
        40,
        None,
        &Default::default(),
        &Default::default(),
    );
    let mut document = file.document.unwrap();
    document.hunks[0].lines[0].spans.push(ByteSpan {
        side: Side::New,
        range: ByteRange {
            start: 0,
            end: 1000,
        },
        syntax: String::new(),
        changed: true,
        move_id: None,
    });
    assert!(document.reconstruct(b"x=1;\n", b"x=2;\n").is_err());
    document.hunks[0].lines[0].spans.pop();
    document.hunks[0].lines[0].new_line = Some(1);
    assert!(document.reconstruct(b"x=1;\n", b"x=2;\n").is_err());
}
#[test]
fn cr_and_invalid_utf8_keep_exact_byte_coordinates() {
    for (old, new) in [
        (b"\xff\r\n\n".as_slice(), b"\xfe\r\n\n".as_slice()),
        (b"".as_slice(), "\r🦋".as_bytes()),
    ] {
        let file = diff::build(
            "x.txt",
            old,
            new,
            10,
            30,
            None,
            &Default::default(),
            &Default::default(),
        );
        file.validate().unwrap();
        assert_eq!(file.document.unwrap().reconstruct(old, new).unwrap(), new);
    }
}
