use mesh_core::lexer::{Piece, Sep, split_line};

#[test]
fn lexer_is_available_to_library_consumers() {
    let segments = split_line("echo 'a b'").expect("line should lex");

    assert_eq!(segments.len(), 1);
    assert_eq!(segments[0].sep_before, Sep::Seq);
    assert_eq!(segments[0].stages.len(), 1);
    assert_eq!(segments[0].stages[0].words.len(), 2);
    assert_eq!(
        segments[0].stages[0].words[1].0,
        vec![Piece::Text {
            text: "a b".to_string(),
            expandable: false,
        }]
    );
}
