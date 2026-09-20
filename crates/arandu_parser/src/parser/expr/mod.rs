mod calls;
mod control;
mod prefix;
mod type_led;

use super::{BinaryOp, CatchHandler, ParseError, ParseErrorCode, Parser, TokenKind, span_between};
use crate::ast::ast_pool::{ExprId, ExprKind};

impl<'a> Parser<'a> {
    pub(super) fn parse_expr(&mut self, min_bp: u8) -> Result<ExprId, ParseError> {
        // Outermost expression only → one EXPR green node (event sink).
        let wrap = min_bp == 0;
        if wrap {
            self.start_node(crate::syntax::SyntaxKind::EXPR);
        }
        let start = self.mark();
        let result = match self.try_parse_expr(min_bp) {
            Ok(expr) => Ok(expr),
            Err(err) => {
                self.report_error(err);
                // No synchronize_expr yet, to avoid eating too much.
                // It just falls back to ExprKind::Error so parent parses can fail/recover naturally.
                let span = self.span_from_mark(start);
                Ok(self.pool.alloc_expr(ExprKind::Error, span))
            }
        };
        if wrap {
            self.finish_node();
        }
        result
    }

    pub(super) fn try_parse_expr(&mut self, min_bp: u8) -> Result<ExprId, ParseError> {
        let mut left = self.parse_prefix()?;
        loop {
            let next_kind = self.current().kind;
            match next_kind {
                TokenKind::Lt => {
                    if self.looks_like_generic_call_or_block_suffix() {
                        let generic_start = self.pool.expr_span(left);
                        let type_range = self.parse_generic_args()?;
                        let span = span_between(generic_start, self.previous().span(self.file_id));
                        left = self.pool.alloc_expr(
                            ExprKind::Generic {
                                callee: left,
                                args: type_range,
                            },
                            span,
                        );

                        if matches!(self.current().kind, TokenKind::LParen) {
                            left = self.finish_call(left)?;
                        } else if self.allow_block_calls
                            && matches!(self.current().kind, TokenKind::LBrace)
                        {
                            left = self.finish_trailing_block_call(left)?;
                        } else if matches!(self.current().kind, TokenKind::KwAs) {
                            // Allowed: `func<T> as ptr[u8]` — loop will handle `as` binary op
                        } else {
                            return Err(ParseError::new(
                                ParseErrorCode::ExpectedToken,
                                "generic block calls are not valid in this context",
                                self.current(),
                                self.file_id,
                                self.source,
                            ));
                        }
                        continue;
                    }
                    if self.looks_like_bare_generic_args()
                        && self.looks_like_generic_args_boundary()
                    {
                        return Err(ParseError::new(
                            ParseErrorCode::ExpectedToken,
                            "generic arguments in expressions must be followed by call arguments or block",
                            self.current(),
                            self.file_id,
                            self.source,
                        ));
                    }
                }
                TokenKind::LParen => {
                    left = self.finish_call(left)?;
                    continue;
                }
                TokenKind::LBrace if self.allow_block_calls => {
                    left = self.finish_trailing_block_call(left)?;
                    continue;
                }
                TokenKind::LBracket => {
                    let span_start = self.pool.expr_span(left);
                    self.advance();
                    let index = self.parse_expr(0)?;
                    self.expect_kind(TokenKind::RBracket)?;
                    let span = span_between(span_start, self.previous().span(self.file_id));
                    left = self
                        .pool
                        .alloc_expr(ExprKind::Index { base: left, index }, span);
                    continue;
                }
                TokenKind::SafeIndexStart => {
                    self.advance();
                    let span_start = self.pool.expr_span(left);
                    let index = self.parse_expr(0)?;
                    self.expect_kind(TokenKind::RBracket)?;
                    let span = span_between(span_start, self.previous().span(self.file_id));
                    left = self
                        .pool
                        .alloc_expr(ExprKind::SafeIndex { base: left, index }, span);
                    continue;
                }
                TokenKind::Dot => {
                    self.advance();
                    let span_start = self.pool.expr_span(left);
                    let field = self.expect_member_name()?;
                    let span = span_between(span_start, self.previous().span(self.file_id));
                    left = self
                        .pool
                        .alloc_expr(ExprKind::Field { base: left, field }, span);
                    continue;
                }
                TokenKind::SafeDot => {
                    self.advance();
                    let span_start = self.pool.expr_span(left);
                    let field = self.expect_member_name()?;
                    let span = span_between(span_start, self.previous().span(self.file_id));
                    left = self
                        .pool
                        .alloc_expr(ExprKind::SafeField { base: left, field }, span);
                    continue;
                }
                TokenKind::Question => {
                    self.advance();
                    let span_start = self.pool.expr_span(left);
                    let span = span_between(span_start, self.previous().span(self.file_id));
                    left = self.pool.alloc_expr(ExprKind::Try { expr: left }, span);
                    continue;
                }
                TokenKind::KwCatch => {
                    let left_bp = 10;
                    let right_bp = 10;
                    if left_bp < min_bp {
                        break;
                    }
                    let span_start = self.pool.expr_span(left);
                    self.advance();
                    let handler = if self.eat_kind(TokenKind::Pipe) {
                        let handler_start = self.pos.saturating_sub(1);
                        let error = self.expect_ident_value()?;
                        self.expect_kind(TokenKind::Pipe)?;
                        let block = self.parse_block()?;
                        let catch_handler = CatchHandler::Block {
                            span: self.span_from_mark(handler_start),
                            error,
                            block,
                        };
                        self.pool.alloc_catch_handler(catch_handler)
                    } else {
                        let handler_start = self.mark();
                        let expr = self.parse_expr(right_bp)?;
                        let catch_handler = CatchHandler::Expr {
                            span: self.span_from_mark(handler_start),
                            expr,
                        };
                        self.pool.alloc_catch_handler(catch_handler)
                    };
                    let span = span_between(span_start, self.previous().span(self.file_id));
                    left = self.pool.alloc_expr(
                        ExprKind::Catch {
                            expr: left,
                            handler,
                        },
                        span,
                    );
                    continue;
                }
                TokenKind::KwAs => {
                    let left_bp = 140;
                    if left_bp < min_bp {
                        break;
                    }
                    let span_start = self.pool.expr_span(left);
                    self.advance();
                    let ty = self.parse_type()?;
                    let span = span_between(span_start, self.pool.type_expr_span(ty));
                    left = self
                        .pool
                        .alloc_expr(ExprKind::Cast { expr: left, ty }, span);
                    continue;
                }
                _ => {}
            }

            let Some((op, left_bp, right_bp)) = self.current_binary() else {
                break;
            };
            if left_bp < min_bp {
                break;
            }
            if matches!(op, BinaryOp::RangeExclusive | BinaryOp::RangeInclusive) {
                let left_kind = self.pool.expr(left);
                if matches!(
                    left_kind,
                    ExprKind::Binary {
                        op: BinaryOp::RangeExclusive | BinaryOp::RangeInclusive,
                        ..
                    }
                ) {
                    return Err(ParseError::new(
                        ParseErrorCode::ExpectedToken,
                        "chained ranges require parentheses",
                        self.current(),
                        self.file_id,
                        self.source,
                    ));
                }
            }
            let span_start = self.pool.expr_span(left);
            self.advance();
            let right = self.parse_expr(right_bp)?;
            let span = span_between(span_start, self.pool.expr_span(right));
            left = if op == BinaryOp::NullCoalesce {
                self.pool
                    .alloc_expr(ExprKind::NullCoalesce { left, right }, span)
            } else {
                self.pool
                    .alloc_expr(ExprKind::Binary { op, left, right }, span)
            };
        }
        Ok(left)
    }
}

#[derive(Clone, Copy)]
struct BinaryOpInfo {
    op: BinaryOp,
    bp: u8,
}

const BINARY_OP_TABLE: [BinaryOpInfo; 21] = [
    BinaryOpInfo {
        op: BinaryOp::NullCoalesce,
        bp: 20,
    }, // 0
    BinaryOpInfo {
        op: BinaryOp::Or,
        bp: 30,
    }, // 1
    BinaryOpInfo {
        op: BinaryOp::And,
        bp: 40,
    }, // 2
    BinaryOpInfo {
        op: BinaryOp::Equal,
        bp: 50,
    }, // 3
    BinaryOpInfo {
        op: BinaryOp::NotEqual,
        bp: 50,
    }, // 4
    BinaryOpInfo {
        op: BinaryOp::Lt,
        bp: 60,
    }, // 5
    BinaryOpInfo {
        op: BinaryOp::Gt,
        bp: 60,
    }, // 6
    BinaryOpInfo {
        op: BinaryOp::LtEqual,
        bp: 60,
    }, // 7
    BinaryOpInfo {
        op: BinaryOp::GtEqual,
        bp: 60,
    }, // 8
    BinaryOpInfo {
        op: BinaryOp::RangeExclusive,
        bp: 70,
    }, // 9
    BinaryOpInfo {
        op: BinaryOp::RangeInclusive,
        bp: 70,
    }, // 10
    BinaryOpInfo {
        op: BinaryOp::BitOr,
        bp: 80,
    }, // 11
    BinaryOpInfo {
        op: BinaryOp::BitXor,
        bp: 90,
    }, // 12
    BinaryOpInfo {
        op: BinaryOp::BitAnd,
        bp: 100,
    }, // 13
    BinaryOpInfo {
        op: BinaryOp::ShiftLeft,
        bp: 110,
    }, // 14
    BinaryOpInfo {
        op: BinaryOp::ShiftRight,
        bp: 110,
    }, // 15
    BinaryOpInfo {
        op: BinaryOp::Add,
        bp: 120,
    }, // 16
    BinaryOpInfo {
        op: BinaryOp::Sub,
        bp: 120,
    }, // 17
    BinaryOpInfo {
        op: BinaryOp::Mul,
        bp: 130,
    }, // 18
    BinaryOpInfo {
        op: BinaryOp::Div,
        bp: 130,
    }, // 19
    BinaryOpInfo {
        op: BinaryOp::Mod,
        bp: 130,
    }, // 20
];

const fn token_kind_index(kind: &TokenKind) -> usize {
    match kind {
        TokenKind::NullCoalesce => 0,
        TokenKind::LogicalOr => 1,
        TokenKind::LogicalAnd => 2,
        TokenKind::EqualEqual => 3,
        TokenKind::BangEqual => 4,
        TokenKind::Lt => 5,
        TokenKind::Gt => 6,
        TokenKind::LtEqual => 7,
        TokenKind::GtEqual => 8,
        TokenKind::RangeExclusive => 9,
        TokenKind::RangeInclusive => 10,
        TokenKind::Pipe => 11,
        TokenKind::Caret => 12,
        TokenKind::Amp => 13,
        TokenKind::ShiftLeft => 14,
        TokenKind::ShiftRight => 15,
        TokenKind::Plus => 16,
        TokenKind::Minus => 17,
        TokenKind::Star => 18,
        TokenKind::Slash => 19,
        TokenKind::Percent => 20,
        _ => 255,
    }
}

impl<'a> Parser<'a> {
    pub(super) fn current_binary(&self) -> Option<(BinaryOp, u8, u8)> {
        let idx = token_kind_index(&self.current().kind);
        if idx < 21 {
            let info = BINARY_OP_TABLE[idx];
            Some((info.op, info.bp, info.bp + 1))
        } else {
            None
        }
    }
}
