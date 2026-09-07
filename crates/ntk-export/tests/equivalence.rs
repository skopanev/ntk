//! Сверка переноса: тот же вход должен давать тот же текст, что даёт
//! работающая реализация на JS.
//!
//! Эталон в fixtures/blocks.json снят ЕЮ, а не написан руками: тест,
//! ожидания которого сочинил тот же человек, что и код, проверяет согласие
//! автора с собой. Расхождение рендера выглядит не как падение, а как тикет,
//! у которого пропала таблица, — поэтому оно ловится здесь, а не в проде.

use serde_json::Value;

#[test]
fn rust_renders_blocks_exactly_as_the_javascript_did() {
    let raw = include_str!("fixtures/blocks.json");
    let cases: Value = serde_json::from_str(raw).expect("фикстуры не разбираются");
    let cases = cases.as_object().expect("ожидался объект случаев");

    let mut mismatched: Vec<String> = Vec::new();
    for (name, case) in cases {
        let blocks: Vec<Value> = case["blocks"].as_array().cloned().unwrap_or_default();
        let expected = case["expected"].as_str().unwrap_or("");
        let got = ntk_export::blocks::blocks_to_text(&blocks, "");
        if got != expected {
            mismatched.push(format!(
                "--- {name} ---\nожидалось:\n{expected}\nполучено:\n{got}"
            ));
        }
    }

    assert!(
        mismatched.is_empty(),
        "рендер разошёлся с исходной реализацией:\n\n{}",
        mismatched.join("\n\n")
    );
}
