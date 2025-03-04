use crate::DumpFileError;
use crate::DumpFileError::ReadError;
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::str;

const COMMENT_CHARS: &str = "--";

pub enum ListQueryResult {
    Continue,
    Break,
}

/// read dump file and callback query function with each valid query inside the dump file
pub fn list_sql_queries_from_dump_file<'a, S, F>(
    dump_file_path: S,
    query: F,
) -> Result<(), DumpFileError>
where
    S: Into<&'a str>,
    F: FnMut(&str) -> ListQueryResult,
{
    let file = match File::open(dump_file_path.into()) {
        Ok(file) => file,
        Err(_) => return Err(DumpFileError::DoesNotExist),
    };

    let reader = BufReader::new(file);
    list_sql_queries_from_dump_reader(reader, query)
}

/// read dump and callback query function with each valid query inside the dump
pub fn list_sql_queries_from_dump_reader<R, F>(
    mut dump_reader: BufReader<R>,
    mut query: F,
) -> Result<(), DumpFileError>
where
    R: Read,
    F: FnMut(&str) -> ListQueryResult,
{
    let mut count_empty_lines = 0;
    let mut buf_bytes: Vec<u8> = Vec::new();
    let mut line_buf_bytes: Vec<u8> = Vec::new();

    loop {
        let bytes = dump_reader.read_until(b'\n', &mut line_buf_bytes);
        let total_bytes = match bytes {
            Ok(bytes) => bytes,
            Err(err) => return Err(ReadError(err)),
        };

        let last_real_char_idx = if buf_bytes.len() > 1 {
            buf_bytes.len() - 2
        } else if buf_bytes.len() == 1 {
            1
        } else {
            0
        };

        // check end of line is a ';' char - it would mean it's the end of the query
        let is_last_line_buf_bytes_by_end_of_query = match line_buf_bytes.get(last_real_char_idx) {
            Some(byte) => *byte == b';',
            None => false,
        };

        let mut query_res = ListQueryResult::Continue;

        buf_bytes.append(&mut line_buf_bytes);

        if total_bytes <= 1 || is_last_line_buf_bytes_by_end_of_query {
            let mut buf_bytes_to_keep: Vec<u8> = Vec::new();

            if buf_bytes.len() > 1 && count_empty_lines == 0 {
                let query_str = str::from_utf8(buf_bytes.as_slice()).unwrap(); // FIXME remove unwrap

                for statement in list_statements(query_str) {
                    match statement {
                        Statement::NewLine => {
                            query("\n");
                        }
                        Statement::CommentLine(comment_statement) => {
                            query(comment_statement.statement);
                        }
                        Statement::Query(sql_statement) => {
                            if sql_statement.valid {
                                query(sql_statement.statement);
                            } else {
                                // the query is not complete, so keep it for the next iteration
                                buf_bytes_to_keep
                                    .extend_from_slice(sql_statement.statement.as_bytes());
                            }
                        }
                    }
                }
            }

            let _ = buf_bytes.clear();
            buf_bytes.extend_from_slice(buf_bytes_to_keep.as_slice());
            count_empty_lines += 1;
        } else {
            count_empty_lines = 0;
        }

        // 49 is an empirical number -
        // not too large to avoid looping too much time, and not too small to avoid wrong end of query
        if count_empty_lines > 49 {
            // EOF?
            break;
        }

        match query_res {
            ListQueryResult::Continue => {}
            ListQueryResult::Break => break,
        }
    }

    Ok(())
}

/// Decodes a hex string to a byte `Vec`.
/// #### example:
///
/// ```rust
/// # use dump_parser::utils::decode_hex;
/// let bytes = decode_hex("0123456789ABCDEF");
/// assert_eq!(bytes, Ok(vec![0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF]));
/// ```
pub fn decode_hex(s: &str) -> Result<Vec<u8>, std::num::ParseIntError> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16))
        .collect()
}

enum Statement<'a> {
    NewLine,
    CommentLine(CommentStatement<'a>),
    Query(QueryStatement<'a>),
}

struct CommentStatement<'a> {
    start_index: usize,
    end_index: usize,
    statement: &'a str,
}

struct QueryStatement<'a> {
    valid: bool,
    start_index: usize,
    end_index: usize,
    statement: &'a str,
}

/// Lightweight function to parse and validate the SQL statement AST.
/// This function can be executed thousands of time per second.
/// It must be fast enough. That's why it does not validate the grammar,
/// but just the structure of a SQL query and return the list of SQL statements with their index
fn list_statements(query: &str) -> Vec<Statement> {
    let mut sql_statements = vec![];
    let mut stack = vec![];

    let mut is_statement_complete = true;
    let mut is_comment_line = false;
    let mut start_index = 0usize;
    for (idx, byte_char) in query.bytes().enumerate() {
        let next_idx = idx + 1;

        match byte_char {
            char if is_comment_line && char == b'\n' => {
                sql_statements.push(Statement::CommentLine(CommentStatement {
                    start_index,
                    end_index: idx,
                    statement: &query[start_index..idx],
                }));

                // set start_index to the current index
                start_index = idx + 1;
                stack.clear();
                is_statement_complete = true;
                is_comment_line = false;
            }
            b'\'' if !is_comment_line => {
                if stack.get(0) == Some(&b'\'') {
                    if (query.len() > next_idx) && &query[next_idx..next_idx] == "'" {
                        // do nothing because the ' char is escaped
                    } else {
                        let _ = stack.remove(0);
                    }
                } else {
                    stack.insert(0, byte_char);
                }
                is_statement_complete = false;
                is_comment_line = false;
            }
            b'(' if !is_comment_line && stack.get(0) != Some(&b'\'') => {
                stack.insert(0, byte_char);
                is_statement_complete = false;
                is_comment_line = false;
            }
            b')' if !is_comment_line => {
                if stack.get(0) == Some(&b'(') {
                    let _ = stack.remove(0);
                } else if stack.get(0) != Some(&b'\'') {
                    stack.insert(0, byte_char);
                }

                is_statement_complete = false;
                is_comment_line = false;
            }
            b'-' if !is_comment_line
                && is_statement_complete
                && (query.len() > next_idx)
                && &query[next_idx..next_idx + 1] == "-" =>
            {
                // comment
                is_comment_line = true;
            }
            b'\n' if !is_comment_line && is_statement_complete => {
                sql_statements.push(Statement::NewLine);
            }
            b';' if !is_comment_line && stack.get(0) != Some(&b'\'') => {
                // end of query
                sql_statements.push(Statement::Query(QueryStatement {
                    valid: stack.is_empty(),
                    start_index,
                    end_index: idx + 1,
                    statement: &query[start_index..idx + 1],
                }));

                // set start_index to the current index
                start_index = idx + 1;
                stack.clear();
                is_statement_complete = true;
                is_comment_line = false;
            }
            _ => {}
        }
    }

    let end_index = query.len() - 1;
    if start_index < end_index {
        if !is_statement_complete {
            sql_statements.push(Statement::Query(QueryStatement {
                valid: stack.is_empty(),
                start_index,
                end_index,
                statement: &query[start_index..end_index + 1],
            }));
        } else if is_comment_line {
            sql_statements.push(Statement::CommentLine(CommentStatement {
                start_index,
                end_index,
                statement: &query[start_index..end_index + 1],
            }));
        } else {
            sql_statements.push(Statement::NewLine);
        }
    }

    sql_statements
}

#[cfg(test)]
mod tests {
    use crate::utils::{list_statements, Statement};

    #[test]
    fn check_list_sql_statements() {
        let s = list_statements(
            "INSERT INTO public.toto (first_name, last_name) VALUES ('john', 'doe');",
        );
        assert_eq!(s.len(), 1);

        match s.get(0).unwrap() {
            Statement::NewLine => {
                assert!(false);
            }
            Statement::CommentLine(_) => {
                assert!(false);
            }
            Statement::Query(s) => {
                assert!(s.valid);
            }
        }

        let s = list_statements(
            "INSERT INTO public.toto (first_name, last_name) VALUES ('jo)hn', 'd(oe');",
        );
        assert_eq!(s.len(), 1);

        match s.get(0).unwrap() {
            Statement::NewLine => {
                assert!(false);
            }
            Statement::CommentLine(_) => {
                assert!(false);
            }
            Statement::Query(s) => {
                assert!(s.valid);
            }
        }

        let s = list_statements(
            "INSERT INTO public.toto (first_name, last_name) VALUES ('john', 'doe'",
        );
        assert_eq!(s.len(), 1);

        match s.get(0).unwrap() {
            Statement::NewLine => {
                assert!(false);
            }
            Statement::CommentLine(_) => {
                assert!(false);
            }
            Statement::Query(s) => {
                assert!(!s.valid);
            }
        }

        let s = list_statements(
            "INSERT INTO public.toto (first_name, last_name)\
                VALUES ('john', 'doe');",
        );
        assert_eq!(s.len(), 1);

        match s.get(0).unwrap() {
            Statement::NewLine => {
                assert!(false);
            }
            Statement::CommentLine(_) => {
                assert!(false);
            }
            Statement::Query(s) => {
                assert!(s.valid);
            }
        }

        let s = list_statements(
            "INSERT INTO public.toto (first_name, last_name) VALUES 'john', 'doe');",
        );
        assert_eq!(s.len(), 1);

        match s.get(0).unwrap() {
            Statement::NewLine => {
                assert!(false);
            }
            Statement::CommentLine(_) => {
                assert!(false);
            }
            Statement::Query(s) => {
                assert!(!s.valid);
            }
        }

        let s = list_statements(
            "INSERT INTO public.toto (first_name, last_name) VALUES ('jo''hn', 'doe');",
        );
        assert_eq!(s.len(), 1);

        match s.get(0).unwrap() {
            Statement::NewLine => {
                assert!(false);
            }
            Statement::CommentLine(_) => {
                assert!(false);
            }
            Statement::Query(s) => {
                assert!(s.valid);
            }
        }

        let s = list_statements(
            "INSERT INTO public.toto (first_name, last_name) VALUES ('jo''hn', 'doe';",
        );
        assert_eq!(s.len(), 1);

        match s.get(0).unwrap() {
            Statement::NewLine => {
                assert!(false);
            }
            Statement::CommentLine(_) => {
                assert!(false);
            }
            Statement::Query(s) => {
                assert!(!s.valid);
            }
        }

        let s = list_statements(
            "INSERT INTO public.toto (first_name, last_name) VALUES\
                ('jo''hn', 'doe');",
        );
        assert_eq!(s.len(), 1);

        match s.get(0).unwrap() {
            Statement::NewLine => {
                assert!(false);
            }
            Statement::CommentLine(_) => {
                assert!(false);
            }
            Statement::Query(s) => {
                assert!(s.valid);
            }
        }

        let s = list_statements(
            "INSERT INTO public.toto (first_name, last_name, description) VALUES\
                ('jo''hn', 'doe', 'wadawdw'';awdawd; awd;awdawdaw rm -rf ;dawd;');",
        );
        assert_eq!(s.len(), 1);

        match s.get(0).unwrap() {
            Statement::NewLine => {
                assert!(false);
            }
            Statement::CommentLine(_) => {
                assert!(false);
            }
            Statement::Query(s) => {
                assert!(s.valid);
            }
        }

        let s = list_statements(
            r#"
--
-- PostgreSQL database dump
--

-- Dumped from database version 12.7
-- Dumped by pg_dump version 14.1

SET statement_timeout = 0;
SET lock_timeout = 0;
SET idle_in_transaction_session_timeout = 0;
SET client_encoding = 'UTF8';
SET standard_conforming_strings = on;
SELECT pg_catalog.set_config('search_path', '', false);
SET check_function_bodies = false;
SET xmloption = content;
SET client_min_messages = warning;
SET row_security = off;

--
-- Name: uuid-ossp; Type: EXTENSION; Schema: -; Owner: -
--

CREATE EXTENSION IF NOT EXISTS "uuid-ossp" WITH SCHEMA public;


--
-- Name: EXTENSION "uuid-ossp"; Type: COMMENT; Schema: -; Owner:
--

COMMENT ON EXTENSION "uuid-ossp" IS 'generate universally unique identifiers (UUIDs)';


SET default_tablespace = '';

SET default_table_access_method = heap;

--
-- Name: toto; Type: TABLE; Schema: public; Owner: admin
--

CREATE TABLE public.toto (
    id uuid DEFAULT uuid_generate_v4() NOT NULL, -- hello world
    created_at timestamp DEFAULT now() NOT NULL
);

--
-- Name: toto; Type: TABLE; Schema: public; Owner: admin
--

CREATE TABLE public.toto2 (
    id uuid DEFAULT uuid_generate_v4() NOT NULL, -- hello world
    created_at timestamp DEFAULT now() NOT NULL
);

"#,
        );

        let mut new_lines = 0usize;
        let mut comments = 0usize;
        let mut sql = vec![];

        for x in s {
            match x {
                Statement::NewLine => {
                    new_lines += 1;
                }
                Statement::CommentLine(_) => {
                    comments += 1;
                }
                Statement::Query(s) => {
                    assert!(s.valid);
                    sql.push(s);
                }
            }
        }

        assert_eq!(new_lines, 33);
        assert_eq!(comments, 17);
        assert_eq!(sql.len(), 16);

        // even if it's not a valid query, the syntax is valid
        let s = list_statements("INSERT INTO public.toto;");
        assert_eq!(s.len(), 1);

        match s.get(0).unwrap() {
            Statement::NewLine => {
                assert!(false);
            }
            Statement::CommentLine(_) => {
                assert!(false);
            }
            Statement::Query(s) => {
                assert!(s.valid);
            }
        }
    }

    #[test]
    fn check_multiple_sql_statements() {
        let s = list_statements("INSERT INTO (first_name, last_name) VALUES ('john', 'doe');SELECT * FROM toto;INSERT INTO (first_name, last_name, age) VALUES ('john', 'doe', 18)");
        assert_eq!(s.len(), 3);

        match s.get(0).unwrap() {
            Statement::NewLine => {
                assert!(false);
            }
            Statement::CommentLine(_) => {
                assert!(false);
            }
            Statement::Query(s) => {
                assert!(s.valid);
            }
        }

        match s.get(1).unwrap() {
            Statement::NewLine => {
                assert!(false);
            }
            Statement::CommentLine(_) => {
                assert!(false);
            }
            Statement::Query(s) => {
                assert!(s.valid);
            }
        }

        match s.get(2).unwrap() {
            Statement::NewLine => {
                assert!(false);
            }
            Statement::CommentLine(_) => {
                assert!(false);
            }
            Statement::Query(s) => {
                assert!(s.valid);
            }
        }

        let s = list_statements("INSERT INTO (first_name, last_name) VALUES ('john', 'doe');SELECT * FROM toto;INSERT INTO (first_name, last_name, age) VALUES ('john', 'doe', 18);");
        assert_eq!(s.len(), 3);

        match s.get(0).unwrap() {
            Statement::NewLine => {
                assert!(false);
            }
            Statement::CommentLine(_) => {
                assert!(false);
            }
            Statement::Query(s) => {
                assert!(s.valid);
            }
        }

        match s.get(1).unwrap() {
            Statement::NewLine => {
                assert!(false);
            }
            Statement::CommentLine(_) => {
                assert!(false);
            }
            Statement::Query(s) => {
                assert!(s.valid);
            }
        }

        match s.get(2).unwrap() {
            Statement::NewLine => {
                assert!(false);
            }
            Statement::CommentLine(_) => {
                assert!(false);
            }
            Statement::Query(s) => {
                assert!(s.valid);
            }
        }

        let s = list_statements("INSERT INTO \n(first_name, last_name) VALUES ('jo\nhn', 'doe');SELECT * FROM toto\n\n;INSERT INTO (first_name, last_name, age) VAL\nUES ('john', 'doe', 18)\n\n\n\n;");
        assert_eq!(s.len(), 6);

        match s.get(0).unwrap() {
            Statement::NewLine => {}
            Statement::CommentLine(_) => {
                assert!(false);
            }
            Statement::Query(s) => {
                assert!(s.valid);
            }
        }

        match s.get(1).unwrap() {
            Statement::NewLine => {
                assert!(false);
            }
            Statement::CommentLine(_) => {
                assert!(false);
            }
            Statement::Query(s) => {
                assert!(s.valid);
            }
        }

        match s.get(2).unwrap() {
            Statement::NewLine => {}
            Statement::CommentLine(_) => {
                assert!(false);
            }
            Statement::Query(s) => {
                assert!(s.valid);
            }
        }

        let s = list_statements("INSERT INTO \n(first_name, last_name VALUES ('jo\nhn', 'do''e');SELECT * FROM toto\n\n;INSERT INTO (first_name, last_name, age) VAL\nUES ('jo''hn', 'doe', 18)\n\n\n\n;");
        assert_eq!(s.len(), 6);

        match s.get(0).unwrap() {
            Statement::NewLine => {}
            Statement::CommentLine(_) => {
                assert!(false);
            }
            Statement::Query(s) => {
                assert!(!s.valid);
            }
        }

        match s.get(1).unwrap() {
            Statement::NewLine => {
                assert!(false);
            }
            Statement::CommentLine(_) => {
                assert!(false);
            }
            Statement::Query(s) => {
                assert!(!s.valid);
            }
        }

        match s.get(2).unwrap() {
            Statement::NewLine => {}
            Statement::CommentLine(_) => {
                assert!(false);
            }
            Statement::Query(s) => {
                assert!(s.valid);
            }
        }
    }
}
