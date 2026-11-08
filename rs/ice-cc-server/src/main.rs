use std::collections::HashMap;
use std::io::{self, Write};

#[derive(Debug, Clone)]
enum Value {
    Number(f64),
    Str(String),
    Nil,
}

fn main() {
    println!("Rust REPL v0.1 — type 'help' for commands, 'quit' to exit");

    let mut vars: HashMap<String, Value> = HashMap::new();

    loop {
        // Print the prompt (flush so it shows immediately)
        print!("> ");
        io::stdout().flush().unwrap();

        // Read a line
        let mut input = String::new();
        if io::stdin().read_line(&mut input).unwrap() == 0 {
            // EOF (Ctrl-D)
            println!("\nBye.");
            break;
        }
        let input = input.trim();

        if input.is_empty() {
            continue;
        }

        // Built-in commands
        match input {
            "quit" | "exit" => {
                println!("Bye.");
                break;
            }
            "help" => {
                println!("Commands:");
                println!("  help          show this message");
                println!("  quit / exit   leave the REPL");
                println!("  vars          list defined variables");
                println!("  <expr>        evaluate an arithmetic expression");
                println!("  let x = <expr> assign a value to a variable");
                continue;
            }
            "vars" => {
                if vars.is_empty() {
                    println!("(no variables)");
                } else {
                    for (k, v) in &vars {
                        println!("  {} = {:?}", k, v);
                    }
                }
                continue;
            }
            _ => {}
        }

        // Handle assignment
        if let Some(rest) = input.strip_prefix("let ") {
            if let Some((name, expr)) = rest.split_once('=') {
                let name = name.trim().to_string();
                match eval(expr.trim(), &vars) {
                    Ok(v) => {
                        println!("{} = {:?}", name, v);
                        vars.insert(name, v);
                    }
                    Err(e) => eprintln!("error: {}", e),
                }
                continue;
            } else {
                eprintln!("error: expected 'let <name> = <expr>'");
                continue;
            }
        }

        // Evaluate expression
        match eval(input, &vars) {
            Ok(v) => println!("{:?}", v),
            Err(e) => eprintln!("error: {}", e),
        }
    }
}

// ---------------------------------------------------------------
// A tiny recursive-descent parser for arithmetic + variables.
//
// Grammar:
//   expr    := term (('+' | '-') term)*
//   term    := factor (('*' | '/') factor)*
//   factor  := number | string | identifier | '(' expr ')' | '-' factor
// ---------------------------------------------------------------

struct Parser<'a> {
    chars: Vec<char>,
    pos: usize,
    vars: &'a HashMap<String, Value>,
}

impl<'a> Parser<'a> {
    fn new(src: &str, vars: &'a HashMap<String, Value>) -> Self {
        Parser {
            chars: src.chars().collect(),
            pos: 0,
            vars,
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }

    fn skip_ws(&mut self) {
        while let Some(c) = self.peek() {
            if c.is_whitespace() {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn parse_expr(&mut self) -> Result<Value, String> {
        let mut lhs = self.parse_term()?;

        loop {
            self.skip_ws();
            match self.peek() {
                Some('+') => {
                    self.bump();
                    let rhs = self.parse_term()?;
                    lhs = add(lhs, rhs)?;
                }
                Some('-') => {
                    self.bump();
                    let rhs = self.parse_term()?;
                    lhs = sub(lhs, rhs)?;
                }
                _ => break,
            }
        }
        Ok(lhs)
    }

    fn parse_term(&mut self) -> Result<Value, String> {
        let mut lhs = self.parse_factor()?;

        loop {
            self.skip_ws();
            match self.peek() {
                Some('*') => {
                    self.bump();
                    let rhs = self.parse_factor()?;
                    lhs = mul(lhs, rhs)?;
                }
                Some('/') => {
                    self.bump();
                    let rhs = self.parse_factor()?;
                    lhs = div(lhs, rhs)?;
                }
                _ => break,
            }
        }
        Ok(lhs)
    }

    fn parse_factor(&mut self) -> Result<Value, String> {
        self.skip_ws();
        match self.peek() {
            Some('(') => {
                self.bump();
                let v = self.parse_expr()?;
                self.skip_ws();
                if self.bump() != Some(')') {
                    return Err("expected ')'".into());
                }
                Ok(v)
            }
            Some('-') => {
                self.bump();
                let v = self.parse_factor()?;
                match v {
                    Value::Number(n) => Ok(Value::Number(-n)),
                    _ => Err("unary '-' requires a number".into()),
                }
            }
            Some(c) if c.is_ascii_digit() || c == '.' => self.parse_number(),
            Some('"') => self.parse_string(),
            Some(c) if c.is_alphabetic() || c == '_' => self.parse_ident(),
            Some(c) => Err(format!("unexpected character '{}'", c)),
            None => Err("unexpected end of input".into()),
        }
    }

    fn parse_number(&mut self) -> Result<Value, String> {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() || c == '.' {
                self.pos += 1;
            } else {
                break;
            }
        }
        let s: String = self.chars[start..self.pos].iter().collect();
        s.parse::<f64>()
            .map(Value::Number)
            .map_err(|_| format!("invalid number '{}'", s))
    }

    fn parse_string(&mut self) -> Result<Value, String> {
        self.bump(); // opening quote
        let mut s = String::new();
        loop {
            match self.bump() {
                Some('"') => return Ok(Value::Str(s)),
                Some('\\') => match self.bump() {
                    Some('n') => s.push('\n'),
                    Some('t') => s.push('\t'),
                    Some('\\') => s.push('\\'),
                    Some('"') => s.push('"'),
                    Some(c) => {
                        s.push('\\');
                        s.push(c);
                    }
                    None => return Err("unterminated string".into()),
                },
                Some(c) => s.push(c),
                None => return Err("unterminated string".into()),
            }
        }
    }

    fn parse_ident(&mut self) -> Result<Value, String> {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c.is_alphanumeric() || c == '_' {
                self.pos += 1;
            } else {
                break;
            }
        }
        let name: String = self.chars[start..self.pos].iter().collect();
        match name.as_str() {
            "nil" => Ok(Value::Nil),
            "true" => Ok(Value::Number(1.0)),
            "false" => Ok(Value::Number(0.0)),
            _ => self
                .vars
                .get(&name)
                .cloned()
                .ok_or_else(|| format!("unknown variable '{}'", name)),
        }
    }
}

fn eval(src: &str, vars: &HashMap<String, Value>) -> Result<Value, String> {
    let mut p = Parser::new(src, vars);
    let v = p.parse_expr()?;
    p.skip_ws();
    if p.pos < p.chars.len() {
        return Err(format!("unexpected trailing input at '{}'", p.chars[p.pos]));
    }
    Ok(v)
}

// ---- helpers for arithmetic + concatenation -------------------------

fn add(a: Value, b: Value) -> Result<Value, String> {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => Ok(Value::Number(x + y)),
        (Value::Str(x), Value::Str(y)) => Ok(Value::Str(x + &y)),
        (Value::Str(x), Value::Number(y)) => Ok(Value::Str(format!("{}{}", x, y))),
        (Value::Number(x), Value::Str(y)) => Ok(Value::Str(format!("{}{}", x, y))),
        _ => Err("cannot add nil".into()),
    }
}

fn sub(a: Value, b: Value) -> Result<Value, String> {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => Ok(Value::Number(x - y)),
        _ => Err("'-' requires numbers".into()),
    }
}

fn mul(a: Value, b: Value) -> Result<Value, String> {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => Ok(Value::Number(x * y)),
        _ => Err("'*' requires numbers".into()),
    }
}

fn div(a: Value, b: Value) -> Result<Value, String> {
    match (a, b) {
        (Value::Number(_), Value::Number(y)) if y == 0.0 => Err("division by zero".into()),
        (Value::Number(x), Value::Number(y)) => Ok(Value::Number(x / y)),
        _ => Err("'/' requires numbers".into()),
    }
}