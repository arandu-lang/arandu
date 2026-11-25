use arandu_diagnostics::{codes::DiagCode, registry};

use super::edit_distance;
use crate::cli_error::{CliResult, CliSuccess};
use crate::pipeline::fail_usage;

fn suggest_code<'a>(code: &str, documented: &'a [DiagCode]) -> Option<&'a str> {
    let mut best: Option<(&'a str, usize)> = None;
    let mut ambiguous = false;
    for candidate in documented {
        let distance = edit_distance(code, candidate.as_str());
        if distance > 2 {
            continue;
        }
        match best {
            None => best = Some((candidate.as_str(), distance)),
            Some((_, best_distance)) if distance < best_distance => {
                best = Some((candidate.as_str(), distance));
                ambiguous = false;
            }
            Some((_, best_distance)) if distance == best_distance => ambiguous = true,
            _ => {}
        }
    }
    if ambiguous {
        None
    } else {
        best.map(|(candidate, _)| candidate)
    }
}

pub fn cmd_explain(code: &str) -> CliResult {
    let documented = DiagCode::ALL
        .iter()
        .copied()
        .filter(|candidate| candidate.requires_error_doc())
        .collect::<Vec<_>>();

    let Some(matched) = DiagCode::ALL
        .iter()
        .copied()
        .find(|candidate| candidate.as_str() == code)
    else {
        let suggestion = suggest_code(code, &documented);
        let hint = suggestion
            .map(|candidate| format!(". Did you mean '{candidate}'?"))
            .unwrap_or_default();
        fail_usage(format!("error: unknown diagnostic code '{code}'{hint}"));
    };

    if !matched.requires_error_doc() {
        fail_usage(format!(
            "error: no long-form explanation for '{}'",
            matched.as_str()
        ));
    }

    let Some(explanation) = registry::get_explanation(matched.as_str()) else {
        fail_usage(format!(
            "error: no long-form explanation for '{}'",
            matched.as_str()
        ));
    };
    println!("{}", explanation.trim_end());
    Ok(CliSuccess::Done)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggestion_skips_ambiguous_ties_and_ice_is_not_explainable() {
        let documented = DiagCode::ALL
            .iter()
            .copied()
            .filter(|code| code.requires_error_doc())
            .collect::<Vec<_>>();
        // T01B is equally close to several T01x codes; guessing one would be
        // misleading, so no suggestion is emitted.
        assert_eq!(suggest_code("T01B", &documented), None);
        assert_eq!(suggest_code("T033XX", &documented), Some("T033"));
        assert!(
            DiagCode::ALL
                .iter()
                .copied()
                .all(|code| !code.requires_error_doc()
                    || registry::get_explanation(code.as_str()).is_some())
        );
    }
}
