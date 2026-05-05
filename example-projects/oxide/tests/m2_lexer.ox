// M2 lexer.ox snapshot test.
//
// Lexes a fixed inline source and prints `(kind, start..end, payload)`
// per token. The snapshot harness captures stdout.

import "stdio.ox";
import "string.ox";
import "../lexer.ox";
import "../util/vec.ox";
import "../util/strbuf.ox";

fn kind_name(k: u8) -> *const [u8] {
    if k == TK_EOF()    { return "EOF";    }
    if k == TK_INT()    { return "Int";    }
    if k == TK_BOOL()   { return "Bool";   }
    if k == TK_CHAR()   { return "Char";   }
    if k == TK_STR()    { return "Str";    }
    if k == TK_IDENT()  { return "Ident";  }
    if k == TK_ERROR()  { return "Error";  }

    if k == TK_KW_FN()       { return "KwFn";       }
    if k == TK_KW_LET()      { return "KwLet";      }
    if k == TK_KW_MUT()      { return "KwMut";      }
    if k == TK_KW_IF()       { return "KwIf";       }
    if k == TK_KW_ELSE()     { return "KwElse";     }
    if k == TK_KW_WHILE()    { return "KwWhile";    }
    if k == TK_KW_LOOP()     { return "KwLoop";     }
    if k == TK_KW_FOR()      { return "KwFor";      }
    if k == TK_KW_RETURN()   { return "KwReturn";   }
    if k == TK_KW_BREAK()    { return "KwBreak";    }
    if k == TK_KW_CONTINUE() { return "KwContinue"; }
    if k == TK_KW_STRUCT()   { return "KwStruct";   }
    if k == TK_KW_ENUM()     { return "KwEnum";     }
    if k == TK_KW_AS()       { return "KwAs";       }
    if k == TK_KW_NULL()     { return "KwNull";     }
    if k == TK_KW_SIZEOF()   { return "KwSizeof";   }
    if k == TK_KW_EXTERN()   { return "KwExtern";   }
    if k == TK_KW_IMPORT()   { return "KwImport";   }
    if k == TK_KW_CONST()    { return "KwConst";    }
    if k == TK_KW_MATCH()    { return "KwMatch";    }
    if k == TK_KW_IMPL()     { return "KwImpl";     }
    if k == TK_KW_TRAIT()    { return "KwTrait";    }
    if k == TK_KW_PUB()      { return "KwPub";      }
    if k == TK_KW_USE()      { return "KwUse";      }
    if k == TK_KW_MOD()      { return "KwMod";      }

    if k == TK_LPAREN()     { return "LParen";     }
    if k == TK_RPAREN()     { return "RParen";     }
    if k == TK_LBRACE()     { return "LBrace";     }
    if k == TK_RBRACE()     { return "RBrace";     }
    if k == TK_LBRACKET()   { return "LBracket";   }
    if k == TK_RBRACKET()   { return "RBracket";   }
    if k == TK_COMMA()      { return "Comma";      }
    if k == TK_SEMI()       { return "Semi";       }
    if k == TK_COLON()      { return "Colon";      }
    if k == TK_COLONCOLON() { return "ColonColon"; }
    if k == TK_ARROW()      { return "Arrow";      }
    if k == TK_DOT()        { return "Dot";        }
    if k == TK_DOTDOT()     { return "DotDot";     }
    if k == TK_DOTDOTDOT()  { return "DotDotDot";  }

    if k == TK_PLUS()      { return "Plus";      }
    if k == TK_MINUS()     { return "Minus";     }
    if k == TK_STAR()      { return "Star";      }
    if k == TK_SLASH()     { return "Slash";     }
    if k == TK_PERCENT()   { return "Percent";   }
    if k == TK_EQ()        { return "Eq";        }
    if k == TK_EQEQ()      { return "EqEq";      }
    if k == TK_NE()        { return "Ne";        }
    if k == TK_LT()        { return "Lt";        }
    if k == TK_LE()        { return "Le";        }
    if k == TK_GT()        { return "Gt";        }
    if k == TK_JOINT_GT()  { return "JointGt";   }
    if k == TK_ANDAND()    { return "AndAnd";    }
    if k == TK_OROR()      { return "OrOr";      }
    if k == TK_BANG()      { return "Bang";      }
    if k == TK_AMP()       { return "Amp";       }
    if k == TK_PIPE()      { return "Pipe";      }
    if k == TK_CARET()     { return "Caret";     }
    if k == TK_TILDE()     { return "Tilde";     }
    if k == TK_SHL()       { return "Shl";       }
    if k == TK_PLUSEQ()    { return "PlusEq";    }
    if k == TK_MINUSEQ()   { return "MinusEq";   }
    if k == TK_STAREQ()    { return "StarEq";    }
    if k == TK_SLASHEQ()   { return "SlashEq";   }
    if k == TK_PERCENTEQ() { return "PercentEq"; }
    if k == TK_AMPEQ()     { return "AmpEq";     }
    if k == TK_PIPEEQ()    { return "PipeEq";    }
    if k == TK_CARETEQ()   { return "CaretEq";   }
    if k == TK_SHLEQ()     { return "ShlEq";     }

    "?"
}

fn err_name(k: u8) -> *const [u8] {
    if k == LE_UNEXPECTED_CHAR()          { return "UnexpectedChar"; }
    if k == LE_UNTERMINATED_BLK_COMMENT() { return "UnterminatedBlockComment"; }
    if k == LE_UNTERMINATED_STRING()      { return "UnterminatedString"; }
    if k == LE_UNTERMINATED_CHAR()        { return "UnterminatedChar"; }
    if k == LE_EMPTY_CHAR()               { return "EmptyChar"; }
    if k == LE_BAD_ESCAPE()               { return "BadEscape"; }
    if k == LE_INT_OVERFLOW()             { return "IntOverflow"; }
    if k == LE_INVALID_DIGIT()            { return "InvalidDigit"; }
    "?"
}

fn dump_tokens(label: *const [u8], lx: *const Lexer) {
    printf("=== %s ===\n", label);
    let n: usize = vec_len::<Token>(&lx.tokens);
    let mut i: usize = 0;
    while i < n {
        let t: Token = vec_get::<Token>(&lx.tokens, i);
        printf("  [%zu..%zu] %s", t.span_start, t.span_end, kind_name(t.kind));
        if t.kind == TK_INT() {
            printf(" %llu", t.int_val);
        } else if t.kind == TK_BOOL() {
            let label_t: *const [u8] = "true";
            let label_f: *const [u8] = "false";
            printf(" %s", if t.bool_val { label_t } else { label_f });
        } else if t.kind == TK_CHAR() {
            printf(" %u", t.char_val);
        } else if t.kind == TK_IDENT() || t.kind == TK_STR() {
            // print payload bytes from pool
            let pool_ptr: *const [u8] = strbuf_as_ptr(&lx.pool);
            // hack: print byte-by-byte to honor str_off/str_len
            printf(" \"");
            let mut j: usize = 0;
            while j < t.str_len {
                let b: u8 = pool_ptr[t.str_off + j];
                printf("%c", b as i32);
                j = j + 1;
            }
            printf("\"");
        } else if t.kind == TK_ERROR() {
            printf(" %s", err_name(t.err_kind));
            if t.err_kind == LE_UNEXPECTED_CHAR() {
                printf("(byte=%u)", t.err_byte as u32);
            }
        }
        printf("\n");
        i = i + 1;
    }
}

fn lex_src(s: *const [u8]) -> Lexer {
    let n: usize = strlen(s);
    lex(s, n)
}

fn run_case(label: *const [u8], src: *const [u8]) {
    let lx: Lexer = lex_src(src);
    dump_tokens(label, &lx);
}

fn main() -> i32 {
    run_case("identifiers + keywords",
             "fn main let mut foo bar_42 _x true false null import sizeof return\n");

    run_case("integers (decimal/hex/binary)",
             "0 1 42 0x1F 0xff 0b1011 1_000_000 0xDEAD_BEEF");

    run_case("integer errors",
             "0xZ 0b 18446744073709551616 0b102");

    run_case("punctuation 1-char",
             "( ) { } [ ] , ; : . + - * / % = < ! & | ^ ~");

    run_case("punctuation multi-char",
             ":: -> .. ... == != <= && || << <<= += -= *= /= %= &= |= ^=");

    run_case("Gt vs JointGt",
             "1 > 2 Foo<Bar<T>> Vec<Vec<i32>>=0");

    run_case("string literal with escapes",
             "\"hi\\n\\t\\x41\\\\\\\"end\"");

    run_case("char literals",
             "'a' '\\n' '\\x7F' '\\\\'");

    run_case("string error: unterminated",
             "\"abc");

    run_case("string error: bad escape",
             "\"a\\zb\"");

    run_case("char errors",
             "'' 'ab' '\\q'");

    run_case("comments (line + block)",
             "let x = 1; // line comment\nlet y = 2; /* nested /* inner */ outer */ let z = 3;");

    run_case("unterminated block comment",
             "/* never closed");

    run_case("unexpected char",
             "@ # $");

    0
}
