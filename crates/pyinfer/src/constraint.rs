//! Port of astroid/constraint.py (4.0.4 has NoneConstraint AND
//! BooleanConstraint in ALL_CONSTRAINT_CLASSES).

use pyast::tree::{ConstValue, NodeKind};

use crate::ctx::Ctx;
use crate::graph::Engine;
use crate::value::{GNode, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConstraintKind {
    /// "is None" / "is not None"
    NoneCheck,
    /// "x" / "not x" truthiness
    Boolean,
}

#[derive(Debug, Clone)]
pub struct Constraint {
    pub kind: ConstraintKind,
    pub negate: bool,
}

/// (the If/IfExp node the constraint came from, constraints)
pub type ConstraintSet = Vec<(GNode, Vec<Constraint>)>;

impl Engine {
    /// constraint.py:167-176 _matches — syntactic equality of two nodes
    /// (Name names, Attribute chains, Const values).
    fn nodes_match(&self, a: GNode, b: GNode) -> bool {
        let ma = self.md(a.m);
        let mb = self.md(b.m);
        match (&ma.tree.nodes[a.n.idx()].kind, &mb.tree.nodes[b.n.idx()].kind) {
            (NodeKind::Name { name: n1 }, NodeKind::Name { name: n2 }) => {
                ma.tree.s(*n1) == mb.tree.s(*n2)
            }
            (
                NodeKind::Attribute {
                    expr: e1,
                    attrname: a1,
                    ..
                },
                NodeKind::Attribute {
                    expr: e2,
                    attrname: a2,
                    ..
                },
            ) => {
                ma.tree.s(*a1) == mb.tree.s(*a2)
                    && self.nodes_match(GNode { m: a.m, n: *e1 }, GNode { m: b.m, n: *e2 })
            }
            (NodeKind::Const(v1), NodeKind::Const(v2)) => v1 == v2,
            _ => false,
        }
    }

    /// node syntactically `is None`-comparable to Const(None)
    fn is_const_none_node(&self, g: GNode) -> bool {
        let md = self.md(g.m);
        matches!(md.tree.nodes[g.n.idx()].kind, NodeKind::Const(ConstValue::None))
    }

    /// constraint.py:179-186 _match_constraint over both constraint classes
    fn match_constraints(&self, node: GNode, expr: GNode, invert: bool) -> Vec<Constraint> {
        let mut out = Vec::new();
        let md = self.md(expr.m);
        // NoneConstraint.match
        if let NodeKind::Compare { left, ops } = &md.tree.nodes[expr.n.idx()].kind {
            if ops.len() == 1 {
                let (op, right) = (&ops[0].0, ops[0].1);
                if (op.as_ref() == "is" || op.as_ref() == "is not")
                    && self.nodes_match(GNode { m: expr.m, n: *left }, node)
                    && self.is_const_none_node(GNode { m: expr.m, n: right })
                {
                    let is_op = op.as_ref() == "is";
                    let negate = (is_op && invert) || (!is_op && !invert);
                    out.push(Constraint {
                        kind: ConstraintKind::NoneCheck,
                        negate,
                    });
                }
            }
        }
        // BooleanConstraint.match
        if self.nodes_match(expr, node) {
            out.push(Constraint {
                kind: ConstraintKind::Boolean,
                negate: invert,
            });
        } else if let NodeKind::UnaryOp { op, operand } = &md.tree.nodes[expr.n.idx()].kind {
            if op.as_ref() == "not"
                && self.nodes_match(GNode { m: expr.m, n: *operand }, node)
            {
                out.push(Constraint {
                    kind: ConstraintKind::Boolean,
                    negate: !invert,
                });
            }
        }
        out
    }

    /// constraint.py:128-155 get_constraints
    pub fn get_constraints(&self, expr: GNode, frame: GNode) -> ConstraintSet {
        let mut mapping: ConstraintSet = Vec::new();
        let mut current = expr;
        while current != frame {
            let parent = match self.parent(current) {
                Some(p) => p,
                None => break,
            };
            let md = self.md(parent.m);
            let test = match &md.tree.nodes[parent.n.idx()].kind {
                NodeKind::If { test, .. } | NodeKind::IfExp { test, .. } => {
                    Some(GNode { m: parent.m, n: *test })
                }
                _ => None,
            };
            if let Some(test) = test {
                let branch = self.locate_child(parent, current);
                let constraints = match branch {
                    Some("body") => Some(self.match_constraints(expr, test, false)),
                    Some("orelse") => Some(self.match_constraints(expr, test, true)),
                    _ => None,
                };
                if let Some(cs) = constraints {
                    if !cs.is_empty() {
                        mapping.push((parent, cs));
                    }
                }
            }
            current = parent;
        }
        mapping
    }

    /// Constraint.satisfied_by
    pub fn constraint_satisfied(&self, c: &Constraint, inferred: &Value, ctx: &std::rc::Rc<Ctx>) -> bool {
        match c.kind {
            ConstraintKind::NoneCheck => {
                if inferred.is_uninferable() {
                    return true;
                }
                let is_none = matches!(self.value_const(inferred), Some(ConstValue::None));
                c.negate ^ is_none
            }
            ConstraintKind::Boolean => {
                if inferred.is_uninferable() {
                    return true;
                }
                // `inferred.bool_value()` — NO context (constraint.py:119):
                // Instance bool_value burns in a fresh counter cell
                let _ = ctx;
                match self.bool_value(inferred, &Ctx::new()) {
                    None => true, // Uninferable boolean
                    Some(b) => c.negate ^ b,
                }
            }
        }
    }
}
