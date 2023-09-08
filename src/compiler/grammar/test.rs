/*
 * Copyright (c) 2020-2023, Stalwart Labs Ltd.
 *
 * This file is part of the Stalwart Sieve Interpreter.
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU Affero General Public License as
 * published by the Free Software Foundation, either version 3 of
 * the License, or (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
 * GNU Affero General Public License for more details.
 * in the LICENSE file at the top-level directory of this distribution.
 * You should have received a copy of the GNU Affero General Public License
 * along with this program.  If not, see <http://www.gnu.org/licenses/>.
 *
 * You can be released from the requirements of the AGPLv3 license by
 * purchasing a commercial license. Please contact licensing@stalw.art
 * for more details.
*/

use serde::{Deserialize, Serialize};

use crate::compiler::{
    lexer::{tokenizer::TokenInfo, word::Word, Token},
    CompileError, ErrorType,
};

use super::{
    actions::{action_convert::Convert, action_vacation::TestVacation},
    expr::{parser::ExpressionParser, tokenizer::Tokenizer, Expression},
    instruction::{CompilerState, Instruction},
    tests::{
        test_address::TestAddress,
        test_body::TestBody,
        test_date::{TestCurrentDate, TestDate},
        test_duplicate::TestDuplicate,
        test_envelope::TestEnvelope,
        test_exists::TestExists,
        test_extlists::TestValidExtList,
        test_hasflag::TestHasFlag,
        test_header::TestHeader,
        test_ihave::TestIhave,
        test_mailbox::{TestMailboxExists, TestMetadata, TestMetadataExists},
        test_mailboxid::TestMailboxIdExists,
        test_notify::{TestNotifyMethodCapability, TestValidNotifyMethod},
        test_plugin::Plugin,
        test_size::TestSize,
        test_spamtest::{TestSpamTest, TestVirusTest},
        test_specialuse::TestSpecialUseExists,
        test_string::TestString,
    },
    Capability, Invalid,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Test {
    True,
    False,
    Address(TestAddress),
    Envelope(TestEnvelope),
    Exists(TestExists),
    Header(TestHeader),
    Size(TestSize),
    Invalid(Invalid),

    // RFC 5173
    Body(TestBody),

    // RFC 6558
    Convert(Convert),

    // RFC 5260
    Date(TestDate),
    CurrentDate(TestCurrentDate),

    // RFC 7352
    Duplicate(TestDuplicate),

    // RFC 5229 & RFC 5183
    String(TestString),
    Environment(TestString),

    // RFC 5435
    NotifyMethodCapability(TestNotifyMethodCapability),
    ValidNotifyMethod(TestValidNotifyMethod),

    // RFC 6134
    ValidExtList(TestValidExtList),

    // RFC 5463
    Ihave(TestIhave),

    // RFC 5232
    HasFlag(TestHasFlag),

    // RFC 5490
    MailboxExists(TestMailboxExists),
    Metadata(TestMetadata),
    MetadataExists(TestMetadataExists),

    // RFC 9042
    MailboxIdExists(TestMailboxIdExists),

    // RFC 5235
    SpamTest(TestSpamTest),
    VirusTest(TestVirusTest),

    // RFC 8579
    SpecialUseExists(TestSpecialUseExists),

    // RFC 5230
    Vacation(TestVacation),

    // Stalwart proprietary
    EvalExpression(EvalExpression),
    Plugin(Plugin),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct EvalExpression {
    pub expr: Vec<Expression>,
    pub is_not: bool,
}

#[derive(Debug)]
struct Block {
    is_all: bool,
    is_not: bool,
    p_count: u32,
    jmps: Vec<usize>,
}

impl<'x> CompilerState<'x> {
    pub(crate) fn parse_test(&mut self) -> Result<(), CompileError> {
        let mut block_stack: Vec<Block> = Vec::new();
        let mut block = Block {
            is_all: false,
            is_not: false,
            p_count: 0,
            jmps: Vec::new(),
        };
        let mut is_not = false;

        loop {
            let token_info = self.tokens.unwrap_next()?;
            self.reset_param_check();
            let test = match token_info.token {
                Token::Comma
                    if !block_stack.is_empty()
                        && matches!(self.instructions.last(), Some(Instruction::Test(_)))
                        && matches!(
                            self.tokens.peek(),
                            Some(Ok(TokenInfo {
                                token: Token::Identifier(_) | Token::Unknown(_),
                                ..
                            }))
                        ) =>
                {
                    is_not = block.is_not;
                    block.jmps.push(self.instructions.len());
                    self.instructions.push(if block.is_all {
                        Instruction::Jz(usize::MAX)
                    } else {
                        Instruction::Jnz(usize::MAX)
                    });
                    continue;
                }
                Token::ParenthesisOpen => {
                    block.p_count += 1;
                    continue;
                }
                Token::ParenthesisClose => {
                    if block.p_count > 0 {
                        block.p_count -= 1;
                        continue;
                    } else if let Some(prev_block) = block_stack.pop() {
                        let cur_pos = self.instructions.len();
                        for jmp_pos in block.jmps {
                            if let Instruction::Jnz(jmp_pos) | Instruction::Jz(jmp_pos) =
                                &mut self.instructions[jmp_pos]
                            {
                                *jmp_pos = cur_pos;
                            } else {
                                debug_assert!(false, "This should not have happened")
                            }
                        }

                        block = prev_block;
                        is_not = block.is_not;
                        if block_stack.is_empty() {
                            break;
                        } else {
                            continue;
                        }
                    } else {
                        return Err(token_info.expected("test name"));
                    }
                }
                Token::Identifier(Word::Not) => {
                    if !matches!(
                        self.tokens.peek(),
                        Some(Ok(TokenInfo {
                            token: Token::Identifier(_) | Token::Unknown(_),
                            ..
                        }))
                    ) {
                        return Err(token_info.expected("test name"));
                    }
                    is_not = !is_not;
                    continue;
                }
                Token::Identifier(word @ (Word::AnyOf | Word::AllOf)) => {
                    if block_stack.len() < self.tokens.compiler.max_nested_tests {
                        self.tokens.expect_token(Token::ParenthesisOpen)?;
                        block_stack.push(block);
                        let (is_all, block_is_not) = if word == Word::AllOf {
                            if !is_not {
                                (true, false)
                            } else {
                                (false, true)
                            }
                        } else if !is_not {
                            (false, false)
                        } else {
                            (true, true)
                        };
                        block = Block {
                            is_all,
                            is_not: block_is_not,
                            p_count: 0,
                            jmps: Vec::new(),
                        };
                        is_not = block_is_not;
                        continue;
                    } else {
                        return Err(CompileError {
                            line_num: token_info.line_num,
                            line_pos: token_info.line_pos,
                            error_type: ErrorType::TooManyNestedTests,
                        });
                    }
                }
                Token::Identifier(Word::True) => {
                    if !is_not {
                        Test::True
                    } else {
                        is_not = false;
                        Test::False
                    }
                }
                Token::Identifier(Word::False) => {
                    if !is_not {
                        Test::False
                    } else {
                        is_not = false;
                        Test::True
                    }
                }
                Token::Identifier(Word::Address) => self.parse_test_address()?,
                Token::Identifier(Word::Envelope) => {
                    self.validate_argument(
                        0,
                        Capability::Envelope.into(),
                        token_info.line_num,
                        token_info.line_pos,
                    )?;
                    self.parse_test_envelope()?
                }
                Token::Identifier(Word::Header) => self.parse_test_header()?,
                Token::Identifier(Word::Size) => self.parse_test_size()?,
                Token::Identifier(Word::Exists) => self.parse_test_exists()?,

                // RFC 5173
                Token::Identifier(Word::Body) => {
                    self.validate_argument(
                        0,
                        Capability::Body.into(),
                        token_info.line_num,
                        token_info.line_pos,
                    )?;
                    self.parse_test_body()?
                }

                // RFC 6558
                Token::Identifier(Word::Convert) => {
                    self.validate_argument(
                        0,
                        Capability::Convert.into(),
                        token_info.line_num,
                        token_info.line_pos,
                    )?;
                    self.parse_test_convert()?
                }

                // RFC 5260
                Token::Identifier(Word::Date) => {
                    self.validate_argument(
                        0,
                        Capability::Date.into(),
                        token_info.line_num,
                        token_info.line_pos,
                    )?;
                    self.parse_test_date()?
                }
                Token::Identifier(Word::CurrentDate) => {
                    self.validate_argument(
                        0,
                        Capability::Date.into(),
                        token_info.line_num,
                        token_info.line_pos,
                    )?;
                    self.parse_test_currentdate()?
                }

                // RFC 7352
                Token::Identifier(Word::Duplicate) => {
                    self.validate_argument(
                        0,
                        Capability::Duplicate.into(),
                        token_info.line_num,
                        token_info.line_pos,
                    )?;
                    self.parse_test_duplicate()?
                }

                // RFC 5229
                Token::Identifier(Word::String) => {
                    self.validate_argument(
                        0,
                        Capability::Variables.into(),
                        token_info.line_num,
                        token_info.line_pos,
                    )?;
                    self.parse_test_string()?
                }

                // RFC 5435
                Token::Identifier(Word::NotifyMethodCapability) => {
                    self.validate_argument(
                        0,
                        Capability::Enotify.into(),
                        token_info.line_num,
                        token_info.line_pos,
                    )?;
                    self.parse_test_notify_method_capability()?
                }
                Token::Identifier(Word::ValidNotifyMethod) => {
                    self.validate_argument(
                        0,
                        Capability::Enotify.into(),
                        token_info.line_num,
                        token_info.line_pos,
                    )?;
                    self.parse_test_valid_notify_method()?
                }

                // RFC 5183
                Token::Identifier(Word::Environment) => {
                    self.validate_argument(
                        0,
                        Capability::Environment.into(),
                        token_info.line_num,
                        token_info.line_pos,
                    )?;
                    self.parse_test_environment()?
                }

                // RFC 6134
                Token::Identifier(Word::ValidExtList) => {
                    self.validate_argument(
                        0,
                        Capability::ExtLists.into(),
                        token_info.line_num,
                        token_info.line_pos,
                    )?;
                    self.parse_test_valid_ext_list()?
                }

                // RFC 5463
                Token::Identifier(Word::Ihave) => {
                    self.validate_argument(
                        0,
                        Capability::Ihave.into(),
                        token_info.line_num,
                        token_info.line_pos,
                    )?;
                    self.parse_test_ihave()?
                }

                // RFC 5232
                Token::Identifier(Word::HasFlag) => {
                    self.validate_argument(
                        0,
                        Capability::Imap4Flags.into(),
                        token_info.line_num,
                        token_info.line_pos,
                    )?;
                    self.parse_test_hasflag()?
                }

                // RFC 5490
                Token::Identifier(Word::MailboxExists) => {
                    self.validate_argument(
                        0,
                        Capability::Mailbox.into(),
                        token_info.line_num,
                        token_info.line_pos,
                    )?;
                    self.parse_test_mailboxexists()?
                }
                Token::Identifier(Word::Metadata) => {
                    self.validate_argument(
                        0,
                        Capability::MboxMetadata.into(),
                        token_info.line_num,
                        token_info.line_pos,
                    )?;
                    self.parse_test_metadata()?
                }
                Token::Identifier(Word::MetadataExists) => {
                    self.validate_argument(
                        0,
                        Capability::MboxMetadata.into(),
                        token_info.line_num,
                        token_info.line_pos,
                    )?;
                    self.parse_test_metadataexists()?
                }
                Token::Identifier(Word::ServerMetadata) => {
                    self.validate_argument(
                        0,
                        Capability::ServerMetadata.into(),
                        token_info.line_num,
                        token_info.line_pos,
                    )?;
                    self.parse_test_servermetadata()?
                }
                Token::Identifier(Word::ServerMetadataExists) => {
                    self.validate_argument(
                        0,
                        Capability::ServerMetadata.into(),
                        token_info.line_num,
                        token_info.line_pos,
                    )?;
                    self.parse_test_servermetadataexists()?
                }

                // RFC 9042
                Token::Identifier(Word::MailboxIdExists) => {
                    self.validate_argument(
                        0,
                        Capability::MailboxId.into(),
                        token_info.line_num,
                        token_info.line_pos,
                    )?;
                    self.parse_test_mailboxidexists()?
                }

                // RFC 5235
                Token::Identifier(Word::SpamTest) => {
                    self.validate_argument(
                        0,
                        Capability::SpamTest.into(),
                        token_info.line_num,
                        token_info.line_pos,
                    )?;
                    self.parse_test_spamtest()?
                }
                Token::Identifier(Word::VirusTest) => {
                    self.validate_argument(
                        0,
                        Capability::VirusTest.into(),
                        token_info.line_num,
                        token_info.line_pos,
                    )?;
                    self.parse_test_virustest()?
                }

                // RFC 8579
                Token::Identifier(Word::SpecialUseExists) => {
                    self.validate_argument(
                        0,
                        Capability::SpecialUse.into(),
                        token_info.line_num,
                        token_info.line_pos,
                    )?;
                    self.parse_test_specialuseexists()?
                }
                Token::Identifier(Word::Eval) => {
                    self.validate_argument(
                        0,
                        Capability::Eval.into(),
                        token_info.line_num,
                        token_info.line_pos,
                    )?;

                    let mut next_token = self.tokens.unwrap_next()?;
                    let expr = match next_token.token {
                        Token::StringConstant(s) => s.into_string().into_bytes(),
                        Token::StringVariable(s) => s,
                        _ => return Err(next_token.expected("string")),
                    };

                    match ExpressionParser::from_tokenizer(Tokenizer::from_iter(
                        expr.iter().enumerate().peekable(),
                        |var_name, maybe_namespace| match self
                            .parse_variable(var_name, maybe_namespace)
                        {
                            Ok(Some(var)) => Ok(var),
                            _ => Err(format!("Invalid variable name {var_name:?}")),
                        },
                    ))
                    .parse()
                    {
                        Ok(parser) => Test::EvalExpression(EvalExpression {
                            expr: parser.output,
                            is_not: false,
                        }),
                        Err(err) => {
                            let err = ErrorType::InvalidExpression(format!(
                                "{}: {}",
                                std::str::from_utf8(&expr).unwrap_or_default(),
                                err
                            ));
                            next_token.token = Token::StringVariable(expr);
                            return Err(next_token.custom(err));
                        }
                    }
                }

                Token::Identifier(word) => {
                    self.ignore_test()?;
                    Test::Invalid(Invalid {
                        name: word.to_string(),
                        line_num: token_info.line_num,
                        line_pos: token_info.line_pos,
                    })
                }
                #[cfg(test)]
                Token::Unknown(name) if name.contains("test") => {
                    use crate::compiler::Value;

                    let mut arguments = Vec::new();
                    arguments.push(crate::PluginArgument::Text(Value::Text(name)));
                    while !matches!(
                        self.tokens.peek().map(|r| r.map(|t| &t.token)),
                        Some(Ok(Token::Comma
                            | Token::ParenthesisClose
                            | Token::CurlyOpen))
                    ) {
                        arguments.push(crate::PluginArgument::Text(
                            match self.tokens.unwrap_next()?.token {
                                Token::StringConstant(s) => Value::from(s),
                                Token::StringVariable(s) => self
                                    .tokenize_string(&s, true)
                                    .map_err(|error_type| CompileError {
                                        line_num: 0,
                                        line_pos: 0,
                                        error_type,
                                    })?,
                                Token::Number(n) => {
                                    Value::Number(crate::compiler::Number::Integer(n as i64))
                                }
                                Token::Identifier(s) => Value::Text(s.to_string()),
                                Token::Tag(s) => Value::Text(format!(":{s}")),
                                Token::Unknown(s) => Value::Text(s),
                                other => panic!("Invalid test param {other:?}"),
                            },
                        ));
                    }
                    Test::Plugin(Plugin {
                        id: u32::MAX,
                        arguments,
                        is_not: false,
                    })
                }
                Token::Unknown(name) => {
                    if let Some(schema) = self.compiler.plugins.get(&name) {
                        self.validate_argument(
                            0,
                            Capability::Plugins.into(),
                            token_info.line_num,
                            token_info.line_pos,
                        )?;
                        self.parse_test_plugin(schema)?
                    } else {
                        self.ignore_test()?;
                        Test::Invalid(Invalid {
                            name,
                            line_num: token_info.line_num,
                            line_pos: token_info.line_pos,
                        })
                    }
                }
                _ => return Err(token_info.expected("test name")),
            };

            while block.p_count > 0 {
                self.tokens.expect_token(Token::ParenthesisClose)?;
                block.p_count -= 1;
            }

            self.instructions.push(Instruction::Test(if !is_not {
                test
            } else {
                test.set_not()
            }));

            if block_stack.is_empty() {
                break;
            }
        }

        self.instructions.push(Instruction::Jz(usize::MAX));
        Ok(())
    }
}

impl Test {
    pub fn set_not(mut self) -> Self {
        match &mut self {
            Test::True => return Test::False,
            Test::False => return Test::True,
            Test::Address(op) => {
                op.is_not = true;
            }
            Test::Envelope(op) => {
                op.is_not = true;
            }
            Test::Exists(op) => {
                op.is_not = true;
            }
            Test::Header(op) => {
                op.is_not = true;
            }
            Test::Size(op) => {
                op.is_not = true;
            }
            Test::Body(op) => {
                op.is_not = true;
            }
            Test::Convert(op) => {
                op.is_not = true;
            }
            Test::Date(op) => {
                op.is_not = true;
            }
            Test::CurrentDate(op) => {
                op.is_not = true;
            }
            Test::Duplicate(op) => {
                op.is_not = true;
            }
            Test::String(op) | Test::Environment(op) => {
                op.is_not = true;
            }
            Test::NotifyMethodCapability(op) => {
                op.is_not = true;
            }
            Test::ValidNotifyMethod(op) => {
                op.is_not = true;
            }
            Test::ValidExtList(op) => {
                op.is_not = true;
            }
            Test::Ihave(op) => {
                op.is_not = true;
            }
            Test::HasFlag(op) => {
                op.is_not = true;
            }
            Test::MailboxExists(op) => {
                op.is_not = true;
            }
            Test::Metadata(op) => {
                op.is_not = true;
            }
            Test::MetadataExists(op) => {
                op.is_not = true;
            }
            Test::MailboxIdExists(op) => {
                op.is_not = true;
            }
            Test::SpamTest(op) => {
                op.is_not = true;
            }
            Test::VirusTest(op) => {
                op.is_not = true;
            }
            Test::SpecialUseExists(op) => {
                op.is_not = true;
            }
            Test::Plugin(op) => {
                op.is_not = true;
            }
            Test::EvalExpression(op) => {
                op.is_not = true;
            }
            Test::Vacation(_) | Test::Invalid(_) => {}
        }
        self
    }
}
