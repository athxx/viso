//! The conversions a value takes where its slot types it: a single-unit length
//! widening to `MixedLength`, a length standing for `fixed` of a `Sizing` or
//! `all` of an `EdgeInsets`, a `Color` standing for `Fill::color` of it, and
//! a value standing for the one-item list of it. Inference records each
//! where it types the value; lowering converts the value's representation
//! there.

use super::InferCx;
use crate::hir::ty::Ty;
use crate::syntax::{SyntaxNode, TextRange};

/// The unit of a single-unit length, in the order of a `MixedLength`'s terms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LengthUnit {
    /// `dp`.
    Dp,
    /// `px`.
    Px,
    /// `sp`.
    Sp,
    /// `em`.
    Em,
    /// `%`.
    Percent,
}

impl LengthUnit {
    /// The unit of a length-family type.
    pub(crate) fn of(ty: &Ty) -> Option<LengthUnit> {
        Some(match ty {
            Ty::Dp => LengthUnit::Dp,
            Ty::Px => LengthUnit::Px,
            Ty::Sp => LengthUnit::Sp,
            Ty::Em => LengthUnit::Em,
            Ty::Percent => LengthUnit::Percent,
            _ => return None,
        })
    }

    /// The length-family type of the unit.
    pub(crate) fn ty(self) -> Ty {
        match self {
            LengthUnit::Dp => Ty::Dp,
            LengthUnit::Px => Ty::Px,
            LengthUnit::Sp => Ty::Sp,
            LengthUnit::Em => Ty::Em,
            LengthUnit::Percent => Ty::Percent,
        }
    }

    /// Its term's index among a `MixedLength`'s five.
    pub(crate) fn term(self) -> usize {
        self as usize
    }
}

/// What a length stands for in a slot that takes one in place of a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StandsFor {
    /// `Sizing::fixed` of it.
    Fixed,
    /// `EdgeInsets::all` of it.
    All,
}

/// How a value converts where it is typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Promotion {
    /// The unit of a single-unit length that widens to `MixedLength`.
    pub(crate) unit: Option<LengthUnit>,
    /// What the (widened) length then stands for.
    pub(crate) stands_for: Option<StandsFor>,
    /// Whether the `Color` stands for `Fill::color` of it.
    pub(crate) fill: bool,
    /// Whether the value (as it converted above) stands for the one-item
    /// list of it.
    pub(crate) listed: bool,
}

impl InferCx<'_> {
    /// How the value of `node` converts where it was typed, if it does.
    pub(crate) fn promotion(&self, node: &SyntaxNode) -> Option<Promotion> {
        self.promotions
            .get(&(node.text_range(), node.kind()))
            .copied()
    }

    /// Notes that the value of `node`, of `produced`, widens to `MixedLength`.
    pub(super) fn note_widening(&mut self, node: &SyntaxNode, produced: &Ty) {
        if let Some(unit) = LengthUnit::of(produced) {
            self.promotions
                .entry((node.text_range(), node.kind()))
                .or_default()
                .unit = Some(unit);
        }
    }

    /// Notes that the length `node` stands for a value as `stands_for`.
    pub(super) fn note_stands_for(&mut self, node: &SyntaxNode, stands_for: StandsFor) {
        self.promotions
            .entry((node.text_range(), node.kind()))
            .or_default()
            .stands_for = Some(stands_for);
    }

    /// Notes that the `Color` `node` stands for `Fill::color` of it.
    pub(super) fn note_fill(&mut self, node: &SyntaxNode) {
        self.promotions
            .entry((node.text_range(), node.kind()))
            .or_default()
            .fill = true;
    }

    /// Notes that the value `node` stands for the one-item list of it.
    pub(super) fn note_listed(&mut self, node: &SyntaxNode) {
        self.promotions
            .entry((node.text_range(), node.kind()))
            .or_default()
            .listed = true;
    }

    /// Forgets the conversions noted within `range`, before it is typed again.
    pub(super) fn forget_promotions(&mut self, range: TextRange) {
        self.promotions
            .retain(|(at, _), _| !range.contains_range(*at));
    }
}
