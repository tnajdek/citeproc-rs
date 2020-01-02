use crate::disamb::names::NameIR;
use crate::prelude::*;
use citeproc_io::Cite;
use csl::Atom;
use std::mem;
use std::sync::{Arc, Mutex};

////////////////////////
// second-field-align //
////////////////////////

impl<O: OutputFormat> IR<O> {
    pub fn split_first_field(&mut self) {
        if let Some(((first, gv), mut me)) = match self {
            IR::Seq(seq) => if seq.contents.len() > 1 {
                Some(seq.contents.remove(0))
            } else {
                None
            }
            .and_then(|f| Some((f, mem::take(seq)))),
            _ => None,
        } {
            me.display = Some(DisplayMode::RightInline);
            let (afpre, afsuf) = me
                .affixes
                .map(|mine| {
                    (
                        Some(Affixes {
                            prefix: mine.prefix,
                            suffix: Atom::from(""),
                        }),
                        Some(Affixes {
                            prefix: Atom::from(""),
                            suffix: mine.suffix,
                        }),
                    )
                })
                .unwrap_or((None, None));
            mem::replace(
                self,
                IR::Seq(IrSeq {
                    contents: vec![
                        (
                            IR::Seq(IrSeq {
                                contents: vec![(first, gv)],
                                display: Some(DisplayMode::LeftMargin),
                                affixes: afpre,
                                ..Default::default()
                            }),
                            gv,
                        ),
                        (
                            IR::Seq(IrSeq {
                                contents: me.contents,
                                display: Some(DisplayMode::RightInline),
                                affixes: afsuf,
                                ..Default::default()
                            }),
                            GroupVars::Important,
                        ),
                    ],
                    display: None,
                    formatting: me.formatting,
                    affixes: None,
                    delimiter: me.delimiter.clone(),
                    dropped_gv: None,
                    quotes: me.quotes.clone(),
                    text_case: me.text_case,
                }),
            );
        }
    }
}

////////////////////////////////
// Cite Grouping & Collapsing //
////////////////////////////////

impl<O: OutputFormat> IR<O> {
    fn first_name_block(&self) -> Option<Arc<Mutex<NameIR<O>>>> {
        match self {
            IR::NameCounter(_) | IR::YearSuffix(..) | IR::Rendered(_) => None,
            IR::Name(ref nir) => Some(nir.clone()),
            IR::ConditionalDisamb(c) => {
                let lock = c.lock().unwrap();
                lock.ir.first_name_block()
            }
            IR::Seq(seq) => {
                // assumes it's the first one that appears
                seq.contents.iter().find_map(|ir| ir.0.first_name_block())
            }
        }
    }
    /// Rest of the name: "if it has a year suffix"
    fn suppress_first_year(&mut self, has_explicit: bool) -> bool {
        match self {
            IR::ConditionalDisamb(c) => {
                let mut lock = c.lock().unwrap();
                lock.ir.suppress_first_year(has_explicit);
                false
            }
            IR::Seq(seq) => {
                let mut found = if seq.contents.len() == 2 {
                    if let ((first, _), (second, gv)) = pair_at_mut(&mut seq.contents, 0).unwrap() {
                        match (second, gv) {
                            (IR::YearSuffix(_), GroupVars::Unresolved) if has_explicit => {
                                *first = IR::Rendered(None);
                                true
                            }
                            (IR::YearSuffix(ys), GroupVars::Important)
                                if !has_explicit && !ys.ir.is_empty() =>
                            {
                                *first = IR::Rendered(None);
                                true
                            }
                            _ => false,
                        }
                    } else {
                        false
                    }
                } else {
                    false
                };
                if !found {
                    for (ir, _) in seq.contents.iter_mut() {
                        if ir.suppress_first_year(has_explicit) {
                            found = true;
                            break;
                        }
                    }
                }
                found
            }
            _ => false,
        }
    }
    fn has_explicit_year_suffix(&self) -> bool {
        match self {
            IR::YearSuffix(YearSuffix {
                hook: YearSuffixHook::Explicit(_),
                ir,
                ..
            }) => !ir.is_empty(),
            IR::ConditionalDisamb(c) => {
                let lock = c.lock().unwrap();
                lock.ir.has_explicit_year_suffix()
            }
            IR::Seq(seq) => {
                // assumes it's the first one that appears
                seq.contents
                    .iter()
                    .any(|ir| ir.0.has_explicit_year_suffix())
            }
            _ => false,
        }
    }
    pub fn suppress_names(&self) {
        if let Some(fnb) = self.first_name_block() {
            let mut guard = fnb.lock().unwrap();
            *guard.ir = IR::Rendered(None);
        }
    }
    pub fn suppress_year(&mut self) {
        let has_explicit = self.has_explicit_year_suffix();
        self.suppress_first_year(has_explicit);
    }
}

impl<O: OutputFormat<Output = String>> IR<O> {
    pub fn collapse_to_cnum(&self, fmt: &O) -> Option<u32> {
        match self {
            IR::Rendered(Some(CiteEdgeData::CitationNumber(build))) => {
                // TODO: just get it from the database
                fmt.output(build.clone(), false).parse().ok()
            }
            IR::ConditionalDisamb(c) => {
                let lock = c.lock().unwrap();
                lock.ir.collapse_to_cnum(fmt)
            }
            IR::Seq(seq) => {
                // assumes it's the first one that appears
                if seq.contents.len() != 1 {
                    None
                } else {
                    seq.contents
                        .first()
                        .and_then(|(x, _)| x.collapse_to_cnum(fmt))
                }
            }
            _ => None,
        }
    }
}

use crate::db::IrGen;
use csl::Collapse;
use std::collections::HashMap;

#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub struct CnumIx {
    pub cnum: u32,
    pub ix: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub enum RangePiece {
    /// If the length of the range is only two, it should be rendered with a comma anyway
    Range(CnumIx, CnumIx),
    Single(CnumIx),
}

impl RangePiece {
    /// Return value is the previous value, to be emitted, if the next it couldn't be appended
    fn attempt_append(&mut self, nxt: CnumIx) -> Option<RangePiece> {
        *self = match self {
            RangePiece::Single(prv) if prv.cnum == nxt.cnum - 1 => RangePiece::Range(*prv, nxt),
            RangePiece::Range(_, end) if end.cnum == nxt.cnum - 1 => {
                *end = nxt;
                return None;
            }
            _ => return Some(std::mem::replace(self, RangePiece::Single(nxt))),
        };
        return None;
    }
}

#[test]
fn range_append() {
    let mut range = RangePiece::Single(CnumIx { cnum: 1, ix: 1 });
    let emit = range.attempt_append(CnumIx { cnum: 2, ix: 2 });
    assert_eq!(
        (range, emit),
        (
            RangePiece::Range(CnumIx { cnum: 1, ix: 1 }, CnumIx { cnum: 2, ix: 2 }),
            None
        )
    );
    let mut range = RangePiece::Single(CnumIx { cnum: 1, ix: 1 });
    let emit = range.attempt_append(CnumIx { cnum: 3, ix: 2 });
    assert_eq!(
        (range, emit),
        (
            RangePiece::Single(CnumIx { cnum: 3, ix: 2 }),
            Some(RangePiece::Single(CnumIx { cnum: 1, ix: 1 }))
        )
    );
}

pub fn collapse_ranges(nums: &[CnumIx]) -> Vec<RangePiece> {
    let mut pieces = Vec::new();
    if let Some(init) = nums.first() {
        let mut wip = RangePiece::Single(*init);
        for &num in &nums[1..] {
            if let Some(emit) = wip.attempt_append(num) {
                pieces.push(emit);
            }
        }
        pieces.push(wip);
    }
    pieces
}

#[test]
fn range_collapse() {
    let s = |cnum: u32| CnumIx {
        cnum,
        ix: cnum as usize,
    };
    assert_eq!(
        collapse_ranges(&[s(1), s(2), s(3)]),
        vec![RangePiece::Range(s(1), s(3))]
    );
    assert_eq!(
        collapse_ranges(&[s(1), s(2), CnumIx { cnum: 4, ix: 3 }]),
        vec![
            RangePiece::Range(s(1), s(2)),
            RangePiece::Single(CnumIx { cnum: 4, ix: 3 })
        ]
    );
}

pub struct Unnamed3<O: OutputFormat> {
    pub cite: Arc<Cite<O>>,
    pub cnum: Option<u32>,
    pub gen4: Arc<IrGen>,
    /// First of a group of cites with the same name
    pub is_first: bool,
    /// Subsequent in a group of cites with the same name
    pub should_collapse: bool,
    /// First of a group of cites with the same year, all with suffixes
    /// (same name implied)
    pub first_of_ys: bool,
    /// Subsequent in a group of cites with the same year, all with suffixes
    /// (same name implied)
    pub collapse_ys: bool,

    /// Ranges of year suffixes (not alphabetic, in its base u32 form)
    pub collapsed_year_suffixes: Vec<RangePiece>,

    /// Ranges of citation numbers
    pub collapsed_ranges: Vec<RangePiece>,

    /// Tagging removed cites is cheaper than memmoving the rest of the Vec
    pub vanished: bool,
}

use std::fmt::{Debug, Formatter};

impl Debug for Unnamed3<Markup> {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        let fmt = &Markup::default();
        f.debug_struct("Unnamed3")
            .field("cite", &self.cite)
            .field("cnum", &self.cnum)
            .field(
                "gen4",
                &self.gen4.ir.flatten(&fmt).map(|x| fmt.output(x, false)),
            )
            .field("is_first", &self.is_first)
            .field("should_collapse", &self.should_collapse)
            .field("first_of_ys", &self.first_of_ys)
            .field("collapse_ys", &self.collapse_ys)
            .field("collapsed_year_suffixes", &self.collapsed_year_suffixes)
            .field("collapsed_ranges", &self.collapsed_ranges)
            .field("vanished", &self.vanished)
            .field("gen4_full", &self.gen4.ir)
            .finish()
    }
}

impl<O: OutputFormat> Unnamed3<O> {
    pub fn new(cite: Arc<Cite<O>>, cnum: Option<u32>, gen4: Arc<IrGen>) -> Self {
        Unnamed3 {
            cite,
            gen4,
            cnum,
            is_first: false,
            should_collapse: false,
            first_of_ys: false,
            collapse_ys: false,
            collapsed_year_suffixes: Vec::new(),
            collapsed_ranges: Vec::new(),
            vanished: false,
        }
    }
}

pub fn group_and_collapse<O: OutputFormat<Output = String>>(
    db: &impl IrDatabase,
    fmt: &Markup,
    delim: &str,
    collapse: Option<Collapse>,
    cites: &mut Vec<Unnamed3<O>>,
) {
    let mut m: HashMap<String, (usize, bool)> = HashMap::new();

    // First, group cites with the same name
    for ix in 0..cites.len() {
        if let Some(rendered) = cites[ix]
            .gen4
            .ir
            .first_name_block()
            .and_then(|fnb| fnb.lock().unwrap().ir.flatten(fmt))
            .map(|flat| fmt.output(flat, false))
        {
            m.entry(rendered)
                .and_modify(|(oix, seen_once)| {
                    // Keep cites separated by affixes together
                    if cites.get(*oix).map_or(false, |u| u.cite.has_suffix())
                        || cites.get(*oix + 1).map_or(false, |u| u.cite.has_prefix())
                        || cites.get(ix - 1).map_or(false, |u| u.cite.has_suffix())
                        || cites.get(ix).map_or(false, |u| u.cite.has_affix())
                    {
                        *oix = ix;
                        return;
                    }
                    if *oix < ix {
                        if !*seen_once {
                            cites[*oix].is_first = true;
                        }
                        *seen_once = true;
                        cites[ix].should_collapse = true;
                        let rotation = &mut cites[*oix + 1..ix + 1];
                        rotation.rotate_right(1);
                        *oix += 1;
                    }
                })
                .or_insert_with(|| (ix, false));
        }
    }

    if collapse == Some(Collapse::CitationNumber) {
        // XXX: Gotta factor in that some might have prefixes and suffixes
        if let Some((first, rest)) = cites.split_first_mut() {
            first.is_first = true;
            for r in rest {
                r.should_collapse = true;
            }
        }
    }

    if let Some(collapse) = collapse {
        match collapse {
            Collapse::CitationNumber => {
                for ix in 0..cites.len() {
                    let slice = &mut cites[ix..];
                    if let Some((u, rest)) = slice.split_first_mut() {
                        if u.is_first {
                            let following = rest.iter_mut().take_while(|u| u.should_collapse);

                            let mut cnums = Vec::new();
                            if let Some(cnum) = u.cnum {
                                cnums.push(CnumIx { cnum, ix });
                            }
                            for (nix, cite) in following.enumerate() {
                                if let Some(cnum) = cite.cnum {
                                    cnums.push(CnumIx {
                                        cnum: cnum,
                                        ix: ix + nix + 1,
                                    });
                                }
                                cite.vanished = true;
                            }
                            u.collapsed_ranges = collapse_ranges(&cnums);
                        }
                    }
                }
            }
            Collapse::Year | Collapse::YearSuffix | Collapse::YearSuffixRanged => {
                let mut ix = 0;
                while ix < cites.len() {
                    let slice = &mut cites[ix..];
                    if let Some((u, rest)) = slice.split_first_mut() {
                        if u.is_first {
                            let following = rest.iter_mut().take_while(|u| u.should_collapse);
                            let mut count = 0;
                            for (nix, cite) in following.enumerate() {
                                let gen4 = Arc::make_mut(&mut cite.gen4);
                                gen4.ir.suppress_names();
                                if collapse != Collapse::Year {
                                    gen4.ir.suppress_year();
                                }
                                count += 1;
                            }
                            ix += count;
                        }
                    }
                    ix += 1;
                }
            }
            _ => {}
        }
    }
}

fn pair_at_mut<T>(mut slice: &mut [T], ix: usize) -> Option<(&mut T, &mut T)> {
    let nix = ix + 1;
    slice = &mut slice[ix..];
    if slice.len() < 2 || nix >= slice.len() {
        return None;
    }
    slice
        .split_first_mut()
        .and_then(|(first, rest)| rest.first_mut().map(|second| (first, second)))
}
