//! The `show` input of the nodes that lay several inputs together (`geo.merge`, `tex.composite`):
//! a chan whose channel k says whether input `in<k>` shows. An input whose channel is 0 or below
//! is left out, and nothing before it cooks for this node. A music stage wires the presence of
//! its elements here, so an element that is not on costs nothing.

use penumbra_core::Chans;
use penumbra_graph::{CookContext, InputChoice, MAX_RUN};

/// The index of the `show` input: right after the run of inputs, which starts at 0.
pub(crate) const SHOW: usize = MAX_RUN;

pub(crate) const SHOW_DOC: &str = "Which inputs show. Channel k is for in<k>, counted in order \
     whatever its name. At 0 or below the input is left out and does not cook. Inputs past the \
     last channel show. Wire a chan.select of the presence channels here, in the order of the \
     inputs";

/// Whether channel `k` hides input `k`.
fn hides(chans: &Chans, k: usize) -> bool {
    chans.channels().get(k).is_some_and(|c| {
        let v = c.value();
        v.is_finite() && v <= 0.0
    })
}

/// The inputs a node with a `show` input needs: every wired one its channel does not hide.
/// Without channels on `show`, every input.
pub(crate) fn choose(cx: &InputChoice<'_>) -> Option<Vec<usize>> {
    let chans = cx.chans(SHOW)?;
    Some((0..MAX_RUN).filter(|&k| cx.connected(k) && !hides(chans, k)).collect())
}

/// Whether `show` hides every wired input: the node then gives out an empty result, not its
/// last one.
pub(crate) fn all_hidden(cx: &CookContext<'_>) -> bool {
    let Some(chans) = cx.input_as::<Chans>(SHOW) else { return false };
    let mut wired = (0..MAX_RUN).filter(|&k| cx.def().inputs.get(k).is_some_and(Option::is_some)).peekable();
    wired.peek().is_some() && wired.all(|k| hides(chans, k))
}

#[cfg(test)]
mod tests {
    use penumbra_core::{Channel, Name};

    use super::*;

    fn chans(values: &[f64]) -> Chans {
        let channels = values
            .iter()
            .enumerate()
            .map(|(i, v)| Channel { name: Name::new(&format!("c{i}")).expect("name"), samples: vec![*v] })
            .collect();
        Chans::new(60.0, channels).expect("chans")
    }

    #[test]
    fn zero_and_below_hide_the_rest_shows() {
        let c = chans(&[0.0, 0.3, -1.0]);
        assert!(hides(&c, 0));
        assert!(!hides(&c, 1));
        assert!(hides(&c, 2));
        assert!(!hides(&c, 3), "past the last channel the input shows");
    }
}
