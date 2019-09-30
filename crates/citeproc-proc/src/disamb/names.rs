use super::add_to_graph;
use super::finite_automata::{Nfa, NfaEdge};
use super::graph_with_stack;
use super::mult_identity;
use crate::names::{NameTokenBuilt, OneNameVar};
use crate::prelude::*;
use citeproc_io::PersonName;
use csl::style::{GivenNameDisambiguationRule, Name as NameEl, NameForm, Names, Style};
use csl::variables::NameVariable;
use csl::Atom;
use fnv::FnvHashMap;
use petgraph::graph::NodeIndex;
use std::sync::Arc;

impl Disambiguation<Markup> for Names {
    fn get_free_conds(&self, db: &impl IrDatabase) -> FreeCondSets {
        // TODO: Position may be involved for NASO and primary disambiguation
        // TODO: drill down into the substitute logic here
        if let Some(subst) = &self.substitute {
            cross_product(db, &subst.0)
        } else {
            mult_identity()
        }
    }
    fn ref_ir(
        &self,
        db: &impl IrDatabase,
        ctx: &RefContext<Markup>,
        stack: Formatting,
    ) -> (RefIR, GroupVars) {
        let fmt = ctx.format;
        let style = ctx.style;
        let _locale = ctx.locale;
        let name_el = db
            .name_citation()
            .merge(self.name.as_ref().unwrap_or(&NameEl::empty()));

        // TODO: resolve which parts of name_el's Formatting are irrelevant due to 'stack'
        // and get a reduced formatting to work with

        let mut runner = OneNameVar {
            name_el: &name_el,
            bump_name_count: 0,
            demote_non_dropping_particle: style.demote_non_dropping_particle,
            initialize_with_hyphen: style.initialize_with_hyphen,
            fmt,
        };

        let mut seq = RefIrSeq {
            contents: Vec::with_capacity(self.variables.len()),
            formatting: self.formatting,
            affixes: self.affixes.clone(),
            delimiter: match &self.delimiter {
                Some(x) => x.0.clone(),
                None => Atom::from(""),
            },
        };

        let name_irs = crate::names::to_individual_name_irs(self, db, fmt, ctx.reference, false);
        for nir in name_irs {
            use crate::names::ntb_len;

            let mut nfa = Nfa::new();
            let start = nfa.graph.add_node(());
            nfa.start.insert(start);
            let mut ntbs =
                runner.names_to_builds(&nir.disamb_names, ctx.position, ctx.locale, &self.et_al);
            let mut max_counted_tokens = 0u16;
            let mut counted_tokens = ntb_len(&ntbs);

            while counted_tokens > max_counted_tokens {
                let one_run = graph_with_stack(
                    db,
                    fmt,
                    &mut nfa,
                    &runner.name_el.formatting,
                    &runner.name_el.affixes,
                    start,
                    |nfa, mut spot| {
                        for ntb in &ntbs {
                            match ntb {
                                NameTokenBuilt::Ratchet(DisambNameRatchet::Literal(b)) => {
                                    if !fmt.is_empty(b) {
                                        let out = fmt.output_in_context(b.clone(), stack);
                                        let e = db.edge(EdgeData::Output(out));
                                        let ir = RefIR::Edge(Some(e));
                                        spot = add_to_graph(db, fmt, nfa, &ir, spot);
                                    }
                                }
                                NameTokenBuilt::Built(b) => {
                                    if !fmt.is_empty(&b) {
                                        let out = fmt.output_in_context(b.to_vec(), stack);
                                        let e = db.edge(EdgeData::Output(out));
                                        let ir = RefIR::Edge(Some(e));
                                        spot = add_to_graph(db, fmt, nfa, &ir, spot);
                                    }
                                }
                                NameTokenBuilt::Ratchet(DisambNameRatchet::Person(ratchet)) => {
                                    let dn = ratchet.data.clone();
                                    spot = add_expanded_name_to_graph(db, nfa, dn, spot, stack);
                                }
                            }
                        }
                        spot
                    },
                );
                if one_run == start {
                    // XXX: not sure about this
                    continue;
                }
                nfa.accepting.insert(one_run);
                runner.bump_name_count += 1;
                ntbs = runner.names_to_builds(
                    &nir.disamb_names,
                    ctx.position,
                    ctx.locale,
                    &self.et_al,
                );
                max_counted_tokens = counted_tokens;
                counted_tokens = ntb_len(&ntbs);
            }
            if !nfa.accepting.is_empty() {
                seq.contents
                    .push(RefIR::Name(RefNameIR::from_name_ir(&nir), nfa))
            }
        }

        if seq.contents.is_empty() {
            // TODO: substitute
            // TODO: suppress once substituted
            return (RefIR::Edge(None), GroupVars::OnlyEmpty);
        }

        (RefIR::Seq(seq), GroupVars::DidRender)
    }
}

citeproc_db::intern_key!(pub DisambName);
impl DisambName {
    pub fn lookup(&self, db: &impl IrDatabase) -> DisambNameData {
        db.lookup_disamb_name(*self)
    }
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct DisambNameData {
    pub ref_id: Atom,
    pub var: NameVariable,
    pub el: NameEl,
    pub value: PersonName,
    pub primary: bool,
}

impl DisambNameData {
    pub fn apply_pass(&mut self, pass: NameDisambPass) {
        match pass {
            NameDisambPass::WithFormLong => self.el.form = Some(NameForm::Long),
            NameDisambPass::WithInitializeFalse => self.el.initialize = Some(false),
        }
    }

    pub(crate) fn single_name(
        &self,
        fmt: &Markup,
        style: &Style,
    ) -> <Markup as OutputFormat>::Build {
        let builder = OneNameVar {
            fmt,
            name_el: &self.el,
            bump_name_count: 0,
            demote_non_dropping_particle: style.demote_non_dropping_particle,
            initialize_with_hyphen: style.initialize_with_hyphen,
        };
        builder.render_person_name(&self.value, !self.primary)
    }

    /// This is used directly for *global name disambiguation*
    pub(crate) fn single_name_edge(&self, db: &impl IrDatabase, stack: Formatting) -> Edge {
        let fmt = &db.get_formatter();
        let style = db.style();
        let built = self.single_name(fmt, &style);
        let o = fmt.output_in_context(built, stack);
        db.edge(EdgeData::Output(o))
    }

    pub fn disamb_iter(&self, rule: GivenNameDisambiguationRule) -> SingleNameDisambIter {
        let method = SingleNameDisambMethod::from_rule(rule, self.primary);
        SingleNameDisambIter::new(method, &self.el)
    }
}

/// The GivenNameDisambiguationRule variants are poorly worded. "-with-initials" doesn't *add*
/// steps, it removes steps / limits the expansion. This is a bit clearer to work with, and mixes
/// in the information about whether a name is primary or not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SingleNameDisambMethod {
    None,
    AddInitials,
    AddInitialsThenGivenName,
}

impl SingleNameDisambMethod {
    /// `is_primary` refers to whether this is the first name to be rendered in a Names element.
    pub fn from_rule(rule: GivenNameDisambiguationRule, is_primary: bool) -> Self {
        match (rule, is_primary) {
            (GivenNameDisambiguationRule::ByCite, _)
            | (GivenNameDisambiguationRule::AllNames, _) => {
                SingleNameDisambMethod::AddInitialsThenGivenName
            }
            (GivenNameDisambiguationRule::AllNamesWithInitials, _) => {
                SingleNameDisambMethod::AddInitials
            }
            (GivenNameDisambiguationRule::PrimaryName, true) => {
                SingleNameDisambMethod::AddInitialsThenGivenName
            }
            (GivenNameDisambiguationRule::PrimaryNameWithInitials, true) => {
                SingleNameDisambMethod::AddInitials
            }
            _ => SingleNameDisambMethod::None,
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct SingleNameDisambIter {
    /// If this is None, the iterator won't produce anything. Essentially the null object
    /// pattern.
    method: SingleNameDisambMethod,
    /// Whether to use part 1 or part 2 of the name expansion steps (confusing, because you are
    /// never running both in sequence, it's a choice)
    initialize_with: bool,
    name_form: NameForm,
    state: NameDisambState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NameDisambState {
    Original,
    AddedInitials,
    AddedGivenName,
}

impl SingleNameDisambIter {
    pub fn new(method: SingleNameDisambMethod, name_el: &NameEl) -> Self {
        SingleNameDisambIter {
            method,
            initialize_with: name_el.initialize_with.is_some() && name_el.initialize == Some(true),
            name_form: name_el.form.unwrap_or(NameForm::Long),
            state: NameDisambState::Original,
        }
    }
}

impl Iterator for SingleNameDisambIter {
    type Item = NameDisambPass;
    fn next(&mut self) -> Option<Self::Item> {
        match self.method {
            SingleNameDisambMethod::None => None,
            SingleNameDisambMethod::AddInitials => {
                if self.initialize_with {
                    match self.state {
                        NameDisambState::Original => {
                            if self.name_form == NameForm::Short {
                                self.state = NameDisambState::AddedInitials;
                                Some(NameDisambPass::WithFormLong)
                            } else {
                                None
                            }
                        }
                        NameDisambState::AddedInitials => None,
                        NameDisambState::AddedGivenName => unreachable!(),
                    }
                } else {
                    None
                }
            }
            SingleNameDisambMethod::AddInitialsThenGivenName => {
                if self.initialize_with {
                    match (self.state, self.name_form) {
                        (NameDisambState::Original, NameForm::Short) => {
                            self.state = NameDisambState::AddedInitials;
                            Some(NameDisambPass::WithFormLong)
                        }
                        (NameDisambState::Original, _) | (NameDisambState::AddedInitials, _) => {
                            self.state = NameDisambState::AddedGivenName;
                            Some(NameDisambPass::WithInitializeFalse)
                        }
                        (NameDisambState::AddedGivenName, _) => None,
                    }
                } else {
                    match self.state {
                        NameDisambState::Original => {
                            self.state = NameDisambState::AddedGivenName;
                            if self.name_form == NameForm::Short {
                                Some(NameDisambPass::WithFormLong)
                            } else {
                                None
                            }
                        }
                        NameDisambState::AddedInitials => unreachable!(),
                        NameDisambState::AddedGivenName => None,
                    }
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameDisambPass {
    WithFormLong,
    WithInitializeFalse,
}

#[cfg(test)]
fn test(name: &NameEl, rule: GivenNameDisambiguationRule, primary: bool) -> Vec<NameDisambPass> {
    let method = SingleNameDisambMethod::from_rule(rule, primary);
    let mut iter = SingleNameDisambIter::new(method, name);
    let passes: Vec<_> = iter.collect();
    passes
}

#[test]
fn test_name_disamb_iter() {
    let mut name = NameEl::root_default();
    name.form = Some(NameForm::Long); // default
    name.initialize = Some(true); // default
    assert_eq!(
        test(&name, GivenNameDisambiguationRule::AllNames, true),
        vec![]
    );

    name.form = Some(NameForm::Short);
    assert_eq!(
        test(&name, GivenNameDisambiguationRule::AllNames, true),
        vec![NameDisambPass::WithFormLong]
    );

    name.form = Some(NameForm::Short);
    assert_eq!(
        test(&name, GivenNameDisambiguationRule::PrimaryName, true),
        vec![NameDisambPass::WithFormLong]
    );
    assert_eq!(
        test(&name, GivenNameDisambiguationRule::PrimaryName, false),
        vec![]
    );
    name.initialize_with = Some(Atom::from("."));
    assert_eq!(
        test(&name, GivenNameDisambiguationRule::AllNames, true),
        vec![
            NameDisambPass::WithFormLong,
            NameDisambPass::WithInitializeFalse
        ]
    );
    assert_eq!(
        test(
            &name,
            GivenNameDisambiguationRule::AllNamesWithInitials,
            true
        ),
        vec![NameDisambPass::WithFormLong]
    );
}

/// Original + expansions
fn add_expanded_name_to_graph(
    db: &impl IrDatabase,
    nfa: &mut Nfa,
    mut dn: DisambNameData,
    spot: NodeIndex,
    stack: Formatting,
) -> NodeIndex {
    let style = db.style();
    let rule = style.citation.givenname_disambiguation_rule;
    let fmt = &db.get_formatter();
    let edge = dn.single_name_edge(db, stack);
    let next_spot = nfa.graph.add_node(());
    let last = add_to_graph(db, fmt, nfa, &RefIR::Edge(Some(edge)), spot);
    nfa.graph.add_edge(last, next_spot, NfaEdge::Epsilon);
    for pass in dn.disamb_iter(rule) {
        dn.apply_pass(pass);
        let first = nfa.graph.add_node(());
        nfa.start.insert(first);
        let edge = dn.single_name_edge(db, stack);
        let last = add_to_graph(db, fmt, nfa, &RefIR::Edge(Some(edge)), spot);
        nfa.graph.add_edge(last, next_spot, NfaEdge::Epsilon);
    }
    next_spot
}

use smallvec::SmallVec;
pub struct NameVariantMatcher(SmallVec<[Edge; 3]>);

impl NameVariantMatcher {
    pub fn accepts(&self, edge: Edge) -> bool {
        self.0.contains(&edge)
    }

    pub fn from_disamb_name(db: &impl IrDatabase, dn: DisambName) -> Self {
        let style = db.style();
        let fmt = &db.get_formatter();
        let rule = style.citation.givenname_disambiguation_rule;

        let mut data: DisambNameData = dn.lookup(db);
        let iter = data.disamb_iter(rule);
        let mut edges = SmallVec::new();
        let edge = data.single_name_edge(db, Formatting::default());
        edges.push(edge);
        for pass in iter {
            data.apply_pass(pass);
            let edge = data.single_name_edge(db, Formatting::default());
            edges.push(edge);
        }
        NameVariantMatcher(edges)
    }
}

/// Performs 'global name disambiguation'
pub fn disambiguated_person_names(
    db: &impl IrDatabase,
) -> Arc<FnvHashMap<DisambName, DisambNameData>> {
    let style = db.style();
    let rule = style.citation.givenname_disambiguation_rule;
    let dagn = style.citation.disambiguate_add_givenname;

    if !dagn || rule == GivenNameDisambiguationRule::ByCite {
        return Arc::new(Default::default());
    }

    let dns = db.all_person_names();
    let fmt = &db.get_formatter();
    let mut matchers = Vec::new();
    let mut results = FnvHashMap::default();

    // preamble: build all the names
    for &dn in dns.iter() {
        matchers.push(NameVariantMatcher::from_disamb_name(db, dn));
    }
    let is_ambiguous = |edge: Edge| -> bool {
        let mut n = 0;
        for m in &matchers {
            let acc = m.accepts(edge);
            if acc {
                n += 1;
            }
            if n > 1 {
                break;
            }
        }
        n > 1
    };

    for &dn_id in dns.iter() {
        let mut dn: DisambNameData = dn_id.lookup(db);
        let mut edge = dn.single_name_edge(db, Formatting::default());
        let mut iter = dn.disamb_iter(rule);
        while is_ambiguous(edge) {
            if let Some(pass) = iter.next() {
                dn.apply_pass(pass);
                edge = dn.single_name_edge(db, Formatting::default());
            } else {
                // failed, so we must reset
                dn = dn_id.lookup(db);
                break;
            }
        }
        results.insert(dn_id, dn);
    }
    Arc::new(results)
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RefNameIR {
    pub variable: NameVariable,
    pub disamb_name_ids: Vec<DisambName>,
}

impl RefNameIR {
    fn from_name_ir<B>(name_ir: &NameIR<B>) -> Self {
        let mut vec = Vec::with_capacity(name_ir.disamb_names.len());
        for dnr in &name_ir.disamb_names {
            match dnr {
                DisambNameRatchet::Person(PersonDisambNameRatchet { id, .. }) => vec.push(*id),
                _ => {}
            }
        }
        RefNameIR {
            variable: name_ir.variable,
            disamb_name_ids: vec,
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct NameIR<B> {
    pub names_el: Names,
    pub variable: NameVariable,
    pub max_name_count: u16,
    pub current_name_count: u16,
    pub bump_name_count: u16,
    pub gn_iter_index: usize,
    pub disamb_names: Vec<DisambNameRatchet<B>>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DisambNameRatchet<B> {
    Literal(B),
    Person(PersonDisambNameRatchet),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonDisambNameRatchet {
    pub id: DisambName,
    pub data: DisambNameData,
    pub iter: SingleNameDisambIter,
}
impl PersonDisambNameRatchet {
    pub fn new(db: &impl IrDatabase, id: DisambName, data: DisambNameData) -> Self {
        let style = db.style();
        let rule = style.citation.givenname_disambiguation_rule;
        let method = SingleNameDisambMethod::from_rule(rule, data.primary);
        let iter = SingleNameDisambIter::new(method, &data.el);
        // debug!("{} ratchet started with state {:?}", &data.ref_id, iter);
        PersonDisambNameRatchet { id, iter, data }
    }
}

impl<B> NameIR<B> {
    pub fn crank(&mut self, pass: Option<DisambPass>) -> bool {
        if let Some(DisambPass::AddNames) = pass {
            self.bump_name_count += 1;
            true
        } else {
            false
        }
    }
}
