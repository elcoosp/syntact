//! Lexer modes with a mode stack (issue 022): per-mode DFA sub-tables
//! driven entirely by data — each [`ModeSpec`] carries its token table
//! plus the [`ModeAction`] taken after matching each token (`Stay`,
//! `Push`, `Pop`, `Switch`), so interpolation blocks, heredocs and
//! nested-comment states are table rows, not scanner code paths.
//!
//! Stack discipline: `Push` enters a nested mode, `Pop` returns to the
//! previous mode, `Switch` replaces the top of the stack, and popping at
//! the base mode is a sticky no-op (the base mode can never be exited).

use gir_ir::TokenPattern;

use crate::dfa::Dfa;
use crate::nfa::Nfa;
use crate::regex::{LexError, Regex, parse_regex};
use crate::scan::Lexeme;

/// What the lexer does with the mode stack after emitting a token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeAction {
    /// Keep the current mode.
    Stay,
    /// Push `mode_id` onto the stack and enter it.
    Push(u16),
    /// Pop back to the previous mode (sticky at the base).
    Pop,
    /// Replace the top of the stack with `mode_id`.
    Switch(u16),
}

/// One token of a mode: global kind, pattern, trivia flag and the action
/// taken after this token matches.
#[derive(Debug, Clone)]
pub struct ModeToken {
    /// The global token kind (the kind carried in [`Lexeme::Token`]).
    pub kind: u16,
    /// The token pattern.
    pub pattern: TokenPattern,
    /// Whether the token is trivia-classified.
    pub trivia: bool,
    /// The mode-stack action applied after this token matches.
    pub action: ModeAction,
}

/// One mode: its token table (compiled to a DFA sub-table) and actions.
#[derive(Debug, Clone)]
pub struct ModeSpec {
    /// The mode's tokens, in declaration order (the tie-break priority).
    pub tokens: Vec<ModeToken>,
}

/// The compiled mode lexer: per-mode DFA sub-tables plus the stack.
#[derive(Debug, Clone)]
pub struct ModeLexer {
    modes: Vec<ModeData>,
    stack: Vec<u16>,
}

#[derive(Debug, Clone)]
struct ModeData {
    dfa: Dfa,
    /// Local DFA accept index → global token kind.
    kinds: Vec<u16>,
    /// Local-indexed trivia flags.
    trivia: Vec<bool>,
    /// Local-indexed mode-stack actions.
    actions: Vec<ModeAction>,
}

impl ModeLexer {
    /// Compile every mode spec into its DFA sub-table. Mode 0 is the base.
    ///
    /// # Errors
    /// [`LexError`] when any token regex fails to compile, or a spec is
    /// empty (a mode that can never match would stall the scanner).
    pub fn new(specs: &[ModeSpec]) -> Result<Self, LexError> {
        if specs.is_empty() {
            return Err(LexError {
                message: "a mode lexer needs at least one mode".into(),
                position: 0,
            });
        }
        let mut modes = Vec::with_capacity(specs.len());
        for spec in specs {
            if spec.tokens.is_empty() {
                return Err(LexError {
                    message: "a mode needs at least one token".into(),
                    position: 0,
                });
            }
            let mut fragments = Vec::with_capacity(spec.tokens.len());
            // The DFA accept carries the token's LOCAL index (position in
            // this mode's list); `kinds` maps local → global kind.
            let mut pattern_of: Vec<u16> = Vec::with_capacity(spec.tokens.len());
            let mut kinds: Vec<u16> = Vec::with_capacity(spec.tokens.len());
            let mut trivia = Vec::with_capacity(spec.tokens.len());
            let mut actions = Vec::with_capacity(spec.tokens.len());
            for (local, token) in spec.tokens.iter().enumerate() {
                let regex: Regex = match &token.pattern {
                    TokenPattern::Lit { text } => literal_regex(text),
                    TokenPattern::Re { regex } => parse_regex(regex)?,
                };
                fragments.push(Nfa::build_fragment(&regex));
                pattern_of.push(u16::try_from(local).expect("mode token count fits u16"));
                kinds.push(token.kind);
                trivia.push(token.trivia);
                actions.push(token.action);
            }
            let nfa = Nfa::merge(&fragments, &pattern_of);
            let dfa = Dfa::from_nfa(&nfa);
            modes.push(ModeData {
                dfa,
                kinds,
                trivia,
                actions,
            });
        }
        Ok(Self {
            modes,
            stack: vec![0],
        })
    }

    /// The current mode id (top of the stack).
    #[must_use]
    pub fn current(&self) -> u16 {
        *self.stack.last().expect("stack never empties")
    }

    /// The mode-stack depth (base mode alone is depth 1).
    #[must_use]
    pub fn depth(&self) -> usize {
        self.stack.len()
    }

    /// Longest match in `mode` at `pos`: `(byte length, global token
    /// kind, action)`. Zero-length accepts are never returned.
    #[must_use]
    pub fn longest_match_in(
        &self,
        mode: u16,
        source: &str,
        pos: usize,
    ) -> Option<(usize, u16, ModeAction)> {
        let data = self.modes.get(mode as usize)?;
        let bytes = source.as_bytes();
        let mut state = data.dfa.start;
        let mut best: Option<(usize, u16)> = None;
        let mut cursor = pos;
        while cursor < bytes.len() {
            let rest = &source[cursor..];
            let c = rest.chars().next()?;
            let class = data.dfa.classes.class_of(c as u32);
            let Some(next) = data.dfa.transitions[state as usize][class] else {
                break;
            };
            state = next;
            cursor += c.len_utf8();
            if let Some(local) = data.dfa.accept[state as usize] {
                best = Some((cursor - pos, local));
            }
        }
        best.map(|(len, local)| {
            (
                len,
                data.kinds[local as usize],
                data.actions[local as usize],
            )
        })
    }

    /// Apply `action` to the stack; returns the mode to continue in.
    fn apply(&mut self, action: ModeAction) -> u16 {
        match action {
            ModeAction::Stay => {}
            ModeAction::Push(mode) => self.stack.push(mode),
            ModeAction::Pop => {
                if self.stack.len() > 1 {
                    self.stack.pop();
                }
            }
            ModeAction::Switch(mode) => {
                *self.stack.last_mut().expect("stack never empties") = mode;
            }
        }
        self.current()
    }

    /// Scan the whole source, driving the mode stack as tokens match.
    /// Unmatched characters are one-char [`Lexeme::Error`] lexemes (progress).
    pub fn scan(&mut self, source: &str) -> Vec<Lexeme> {
        let bytes = source.as_bytes();
        let mut lexemes = Vec::new();
        let mut pos = 0_usize;
        while pos < bytes.len() {
            let matched = self.longest_match_in(self.current(), source, pos);
            let Some((len, kind, action)) = matched else {
                let end = char_end(source, pos);
                lexemes.push(Lexeme::Error {
                    range: as_u32(pos)..as_u32(end),
                });
                pos = end;
                continue;
            };
            let end = pos + len;
            let current = self.current();
            // Recover the local index for the mode-local trivia table.
            let local = self.modes[current as usize]
                .kinds
                .iter()
                .position(|&k| k == kind)
                .unwrap_or(0);
            lexemes.push(Lexeme::Token {
                kind,
                range: as_u32(pos)..as_u32(end),
                trivia: self.modes[current as usize].trivia[local],
            });
            self.apply(action);
            pos = end;
        }
        lexemes
    }

    /// The number of modes.
    #[must_use]
    pub fn mode_count(&self) -> usize {
        self.modes.len()
    }
}

fn literal_regex(text: &str) -> Regex {
    Regex::Concat(
        text.chars()
            .map(|c| Regex::Class(vec![(c as u32, c as u32)]))
            .collect(),
    )
}

fn char_end(source: &str, pos: usize) -> usize {
    let mut end = pos + 1;
    while end < source.len() && !source.is_char_boundary(end) {
        end += 1;
    }
    end
}

fn as_u32(v: usize) -> u32 {
    u32::try_from(v).expect("offset fits u32")
}
