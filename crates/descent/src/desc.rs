//! Grammar descriptors (issue 023): the grammar as data. A [`Desc`] is a
//! dense rule table; rule bodies are [`Pat`] combinators; expressions
//! reference [`PrattDesc`] tables by id. Descriptors are the codegen
//! input shape (issue 015 emits tables from exactly this data) — the
//! interpreter and the generated parser must agree by construction.

/// A node-wrapping pattern: `Node(kind, inner)` emits
/// `Start(kind) … Finish` around `inner`.
#[derive(Debug, Clone)]
pub enum Pat {
    /// Match one token of `kind` from the feed (emits a Token event).
    Tok(u16),
    /// Match all children in order; first mismatch rolls the whole
    /// sequence back.
    Seq(Vec<Pat>),
    /// Try children in order; the first match commits. All failing rolls
    /// the alternative back.
    Alt(Vec<Pat>),
    /// Optional child: on mismatch, match empty (zero-width).
    Opt(Box<Pat>),
    /// Zero-or-more children. An iteration that consumes no token breaks
    /// the loop (the progress guarantee — recovery never spins).
    Rep(Box<Pat>),
    /// Wrap `inner` in a node of `kind`.
    Node(u16, Box<Pat>),
    /// Delegate to rule `id` (its node kind + mode hook apply).
    Rule(u16),
    /// Run the Pratt expression table `id` (binding-power climbing).
    Expr(u16),
    /// LL(1) dispatch guard: match `pat` only when the current feed token
    /// is `tok` (without consuming it) — how generated descent selects
    /// rule alternatives without backtracking past tokens.
    Peek {
        /// The required lookahead token kind.
        tok: u16,
        /// The guarded pattern.
        pat: Box<Pat>,
    },
    /// FIRST-set dispatch guard: match `pat` only when the current feed
    /// token is one of `toks` (without consuming) — the multi-token form
    /// of [`Pat::Peek`], driving optional/repeated lists (e.g. array
    /// elements) without speculatively entering self-recovering rules.
    PeekSet {
        /// The FIRST-set lookahead token kinds.
        toks: Vec<u16>,
        /// The guarded pattern.
        pat: Box<Pat>,
    },
    /// Recovery step (issue 023 v0; calibrated in issue 026): emit an
    /// error + ERROR node around the offending token (empty at EOF) and
    /// consume it — always "matches", guaranteeing forward progress.
    Recover,
}

/// One rule of a descriptor grammar.
#[derive(Debug, Clone)]
pub struct RuleDesc {
    /// The node kind emitted for this rule.
    pub node_kind: u16,
    /// The rule body.
    pub body: Pat,
    /// Token kinds recovery must never consume (the enclosing closers) —
    /// the sync set; calibration is issue 026.
    pub sync: Vec<u16>,
    /// The mode monomorphization pin: the lexer mode (issue 022) this
    /// rule's region is scanned in, if the grammar is modal.
    pub mode_hook: Option<u16>,
}

/// A Pratt expression table (issue 023): atoms, prefix/postfix unary
/// mix-ins and infix binary operators with binding powers.
#[derive(Debug, Clone)]
pub struct PrattDesc {
    /// The bare atom pattern (operands) — must emit a self-contained
    /// (balanced) event span, e.g. `Node(NUM, Tok(INT))`.
    pub atom: Pat,
    /// Transparent group `(open, close)` (parenthesized sub-expressions):
    /// no node of its own — the inner climb's shape stands for the group.
    pub group: Option<(u16, u16)>,
    /// Prefix unaries: `Start(node) token operand Finish`.
    pub prefix: Vec<UnaryDesc>,
    /// Postfix unaries: `Start(node) operand token Finish`.
    pub postfix: Vec<UnaryDesc>,
    /// Infix binaries: `Start(node) lhs token rhs Finish` with
    /// `(left, right)` binding powers — `right == left + 1` encodes
    /// left associativity, `right == left` right associativity.
    pub infix: Vec<InfixDesc>,
}

/// A prefix or postfix unary descriptor.
#[derive(Debug, Clone, Copy)]
pub struct UnaryDesc {
    /// The operator token kind.
    pub token: u16,
    /// The node kind wrapping operator + operand.
    pub node: u16,
    /// Binding power handed to the operand climb.
    pub bp: u8,
}

/// An infix binary descriptor.
#[derive(Debug, Clone, Copy)]
pub struct InfixDesc {
    /// The operator token kind.
    pub token: u16,
    /// Left binding power (minimum to keep climbing on the left).
    pub lbp: u8,
    /// Right binding power (minimum for the right operand) — the
    /// associativity encoding.
    pub rbp: u8,
    /// The node kind wrapping `lhs op rhs`.
    pub node: u16,
}

/// A descriptor grammar: dense rules + expression tables.
#[derive(Debug, Clone)]
pub struct Desc {
    /// Rules, dense by id (`Pat::Rule(id)` indexes this).
    pub rules: Vec<RuleDesc>,
    /// Pratt tables, dense by id (`Pat::Expr(id)` indexes this).
    pub exprs: Vec<PrattDesc>,
    /// The start pattern (the document shape — emitted without a
    /// wrapping node, mirroring the no-implicit-root contract).
    pub start: Pat,
}

impl Desc {
    /// The rule for `id` (panics on an out-of-bounds id — descriptors are
    /// built by the pack, validated at construction).
    ///
    /// # Panics
    /// When `id >= rules.len()`.
    #[must_use]
    pub fn rule(&self, id: u16) -> &RuleDesc {
        &self.rules[usize::from(id)]
    }

    /// The Pratt table for `id`.
    ///
    /// # Panics
    /// When `id >= exprs.len()`.
    #[must_use]
    pub fn expr(&self, id: u16) -> &PrattDesc {
        &self.exprs[usize::from(id)]
    }
}
