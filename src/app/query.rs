use super::ProcWidgetState;
use crate::{
    data_conversion::ConvertedProcessData,
    utils::error::{
        BottomError::{self, QueryError},
        Result,
    },
};
use std::collections::VecDeque;

const DELIMITER_LIST: [char; 5] = ['=', '>', '<', '(', ')'];
const AND_LIST: [&str; 2] = ["and", "&&"];
const OR_LIST: [&str; 2] = ["or", "||"];

/// I only separated this as otherwise, the states.rs file gets huge... and this should
/// belong in another file anyways, IMO.
pub trait ProcessQuery {
    /// In charge of parsing the given query.
    /// We are defining the following language for a query (case-insensitive prefixes):
    ///
    /// - Process names: No prefix required, can use regex, match word, or case.
    ///   Enclosing anything, including prefixes, in quotes, means we treat it as an entire process
    ///   rather than a prefix.
    /// - PIDs: Use prefix `pid`, can use regex or match word (case is irrelevant).
    /// - CPU: Use prefix `cpu`, cannot use r/m/c (regex, match word, case).  Can compare.
    /// - MEM: Use prefix `mem`, cannot use r/m/c.  Can compare.
    /// - STATE: Use prefix `state`, TODO when we update how state looks in 0.5 probably.
    /// - Read/s: Use prefix `r`.  Can compare.
    /// - Write/s: Use prefix `w`.  Can compare.
    /// - Total read: Use prefix `read`.  Can compare.
    /// - Total write: Use prefix `write`.  Can compare.
    ///
    /// For queries, whitespaces are our delimiters.  We will merge together any adjacent non-prefixed
    /// or quoted elements after splitting to treat as process names.
    /// Furthermore, we want to support boolean joiners like AND and OR, and brackets.
    fn parse_query(&self) -> Result<Query>;
}

impl ProcessQuery for ProcWidgetState {
    fn parse_query(&self) -> Result<Query> {
        fn process_string_to_filter(query: &mut VecDeque<String>) -> Result<Query> {
            Ok(Query {
                query: process_and(query)?,
            })
        }

        fn process_and(query: &mut VecDeque<String>) -> Result<And> {
            let mut lhs = process_or(query)?;
            let mut rhs: Option<Box<Or>> = None;

            while let Some(queue_top) = query.front() {
                if AND_LIST.contains(&queue_top.to_lowercase().as_str()) {
                    query.pop_front();
                    rhs = Some(Box::new(process_or(query)?));

                    if let Some(queue_next) = query.front() {
                        if AND_LIST.contains(&queue_next.to_lowercase().as_str()) {
                            // Must merge LHS and RHS
                            lhs = Or {
                                lhs: Prefix {
                                    and: Some(Box::new(And { lhs, rhs })),
                                    regex_prefix: None,
                                    compare_prefix: None,
                                },
                                rhs: None,
                            };
                            rhs = None;
                        }
                    } else {
                        break;
                    }
                } else {
                    break;
                }
            }

            Ok(And { lhs, rhs })
        }

        fn process_or(query: &mut VecDeque<String>) -> Result<Or> {
            let mut lhs = process_prefix(query)?;
            let mut rhs: Option<Box<Prefix>> = None;

            while let Some(queue_top) = query.front() {
                if OR_LIST.contains(&queue_top.to_lowercase().as_str()) {
                    query.pop_front();
                    rhs = Some(Box::new(process_prefix(query)?));

                    if let Some(queue_next) = query.front() {
                        if OR_LIST.contains(&queue_next.to_lowercase().as_str()) {
                            // Must merge LHS and RHS
                            lhs = Prefix {
                                and: Some(Box::new(And {
                                    lhs: Or { lhs, rhs },
                                    rhs: None,
                                })),
                                regex_prefix: None,
                                compare_prefix: None,
                            };
                            rhs = None;
                        }
                    } else {
                        break;
                    }
                } else {
                    break;
                }
            }

            Ok(Or { lhs, rhs })
        }

        fn process_prefix(query: &mut VecDeque<String>) -> Result<Prefix> {
            if let Some(queue_top) = query.pop_front() {
                if queue_top == "(" {
                    // Get content within bracket; and check if paren is complete
                    let and = process_and(query)?;
                    if let Some(close_paren) = query.pop_front() {
                        if close_paren.to_lowercase() == ")" {
                            return Ok(Prefix {
                                and: Some(Box::new(and)),
                                regex_prefix: None,
                                compare_prefix: None,
                            });
                        } else {
                            return Err(QueryError("Missing closing parentheses".into()));
                        }
                    } else {
                        return Err(QueryError("Missing closing parentheses".into()));
                    }
                } else if queue_top == ")" {
                    // This is actually caught by the regex creation, but it seems a bit
                    // sloppy to leave that up to that to do so...

                    return Err(QueryError("Missing opening parentheses".into()));
                } else {
                    //  Get prefix type...
                    let prefix_type = queue_top.parse::<PrefixType>()?;
                    let content = if let PrefixType::Name = prefix_type {
                        Some(queue_top)
                    } else {
                        query.pop_front()
                    };

                    if let Some(content) = content {
                        match &prefix_type {
                            PrefixType::Name => {
                                return Ok(Prefix {
                                    and: None,
                                    regex_prefix: Some((
                                        prefix_type,
                                        StringQuery::Value(content.trim_matches('\"').to_owned()),
                                    )),
                                    compare_prefix: None,
                                })
                            }
                            PrefixType::Pid => {
                                // We have to check if someone put an "="...
                                if content == "=" {
                                    // Check next string if possible
                                    if let Some(queue_next) = query.pop_front() {
                                        return Ok(Prefix {
                                            and: None,
                                            regex_prefix: Some((
                                                prefix_type,
                                                StringQuery::Value(queue_next),
                                            )),
                                            compare_prefix: None,
                                        });
                                    }
                                } else {
                                    return Ok(Prefix {
                                        and: None,
                                        regex_prefix: Some((
                                            prefix_type,
                                            StringQuery::Value(content),
                                        )),
                                        compare_prefix: None,
                                    });
                                }
                            }
                            _ => {
                                // Now we gotta parse the content... yay.

                                let mut condition: Option<QueryComparison> = None;
                                let mut value: Option<f64> = None;

                                if content == "=" {
                                    // TODO: Do we want to allow just an empty space to work here too?  ie: cpu 5?
                                    condition = Some(QueryComparison::Equal);
                                    if let Some(queue_next) = query.pop_front() {
                                        value = queue_next.parse::<f64>().ok();
                                    }
                                } else if content == ">" || content == "<" {
                                    // We also have to check if the next string is an "="...
                                    if let Some(queue_next) = query.pop_front() {
                                        if queue_next == "=" {
                                            condition = Some(if content == ">" {
                                                QueryComparison::GreaterOrEqual
                                            } else {
                                                QueryComparison::LessOrEqual
                                            });
                                            if let Some(queue_next_next) = query.pop_front() {
                                                value = queue_next_next.parse::<f64>().ok();
                                            }
                                        } else {
                                            condition = Some(if content == ">" {
                                                QueryComparison::Greater
                                            } else {
                                                QueryComparison::Less
                                            });
                                            value = queue_next.parse::<f64>().ok();
                                        }
                                    }
                                }

                                if let Some(condition) = condition {
                                    if let Some(read_value) = value {
                                        // Now we want to check one last thing - is there a unit?
                                        // If no unit, assume base.
                                        // Furthermore, base must be PEEKED at initially, and will
                                        // require (likely) prefix_type specific checks
                                        // Lastly, if it *is* a unit, remember to POP!

                                        let mut value = read_value;

                                        match prefix_type {
                                            PrefixType::Rps
                                            | PrefixType::Wps
                                            | PrefixType::TRead
                                            | PrefixType::TWrite => {
                                                if let Some(potential_unit) = query.front() {
                                                    match potential_unit.as_str() {
                                                        "TB" => {
                                                            value *= 1_000_000_000_000.0;
                                                            query.pop_front();
                                                        }
                                                        "TiB" => {
                                                            value *= 1_099_511_627_776.0;
                                                            query.pop_front();
                                                        }
                                                        "GB" => {
                                                            value *= 1_000_000_000.0;
                                                            query.pop_front();
                                                        }
                                                        "GiB" => {
                                                            value *= 1_073_741_824.0;
                                                            query.pop_front();
                                                        }
                                                        "MB" => {
                                                            value *= 1_000_000.0;
                                                            query.pop_front();
                                                        }
                                                        "MiB" => {
                                                            value *= 1_048_576.0;
                                                            query.pop_front();
                                                        }
                                                        "KB" => {
                                                            value *= 1000.0;
                                                            query.pop_front();
                                                        }
                                                        "KiB" => {
                                                            value *= 1024.0;
                                                            query.pop_front();
                                                        }
                                                        "B" => {
                                                            // Just gotta pop.
                                                            query.pop_front();
                                                        }
                                                        _ => {}
                                                    }
                                                }
                                            }
                                            _ => {}
                                        }

                                        return Ok(Prefix {
                                            and: None,
                                            regex_prefix: None,
                                            compare_prefix: Some((
                                                prefix_type,
                                                NumericalQuery { condition, value },
                                            )),
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
            }

            Err(QueryError("Failed to parse comparator.".into()))
        }

        let mut split_query = VecDeque::new();

        self.get_current_search_query()
            .split_whitespace()
            .for_each(|s| {
                // From https://stackoverflow.com/a/56923739 in order to get a split but include the parentheses
                let mut last = 0;
                for (index, matched) in s.match_indices(|x| DELIMITER_LIST.contains(&x)) {
                    if last != index {
                        split_query.push_back(s[last..index].to_owned());
                    }
                    split_query.push_back(matched.to_owned());
                    last = index + matched.len();
                }
                if last < s.len() {
                    split_query.push_back(s[last..].to_owned());
                }
            });

        let mut process_filter = process_string_to_filter(&mut split_query)?;
        process_filter.process_regexes(
            self.process_search_state.is_searching_whole_word,
            self.process_search_state.is_ignoring_case,
            self.process_search_state.is_searching_with_regex,
        )?;

        Ok(process_filter)
    }
}

#[derive(Debug)]
pub struct Query {
    pub query: And,
}

impl Query {
    pub fn process_regexes(
        &mut self, is_searching_whole_word: bool, is_ignoring_case: bool,
        is_searching_with_regex: bool,
    ) -> Result<()> {
        self.query.process_regexes(
            is_searching_whole_word,
            is_ignoring_case,
            is_searching_with_regex,
        )
    }

    pub fn check(&self, process: &ConvertedProcessData) -> bool {
        self.query.check(process)
    }
}

#[derive(Debug)]
pub struct And {
    pub lhs: Or,
    pub rhs: Option<Box<Or>>,
}

impl And {
    pub fn process_regexes(
        &mut self, is_searching_whole_word: bool, is_ignoring_case: bool,
        is_searching_with_regex: bool,
    ) -> Result<()> {
        self.lhs.process_regexes(
            is_searching_whole_word,
            is_ignoring_case,
            is_searching_with_regex,
        )?;
        if let Some(rhs) = &mut self.rhs {
            rhs.process_regexes(
                is_searching_whole_word,
                is_ignoring_case,
                is_searching_with_regex,
            )?;
        }

        Ok(())
    }

    pub fn check(&self, process: &ConvertedProcessData) -> bool {
        if let Some(rhs) = &self.rhs {
            self.lhs.check(process) && rhs.check(process)
        } else {
            self.lhs.check(process)
        }
    }
}

#[derive(Debug)]
pub struct Or {
    pub lhs: Prefix,
    pub rhs: Option<Box<Prefix>>,
}

impl Or {
    pub fn process_regexes(
        &mut self, is_searching_whole_word: bool, is_ignoring_case: bool,
        is_searching_with_regex: bool,
    ) -> Result<()> {
        self.lhs.process_regexes(
            is_searching_whole_word,
            is_ignoring_case,
            is_searching_with_regex,
        )?;
        if let Some(rhs) = &mut self.rhs {
            rhs.process_regexes(
                is_searching_whole_word,
                is_ignoring_case,
                is_searching_with_regex,
            )?;
        }

        Ok(())
    }

    pub fn check(&self, process: &ConvertedProcessData) -> bool {
        if let Some(rhs) = &self.rhs {
            self.lhs.check(process) || rhs.check(process)
        } else {
            self.lhs.check(process)
        }
    }
}

#[derive(Debug)]
pub enum PrefixType {
    Pid,
    Cpu,
    Mem,
    Rps,
    Wps,
    TRead,
    TWrite,
    Name,
    __Nonexhaustive,
}

impl std::str::FromStr for PrefixType {
    type Err = BottomError;

    fn from_str(s: &str) -> Result<Self> {
        use PrefixType::*;

        let lower_case = s.to_lowercase();
        match lower_case.as_str() {
            "cpu" => Ok(Cpu),
            "mem" => Ok(Mem),
            "r" => Ok(Rps),
            "w" => Ok(Wps),
            "read" => Ok(TRead),
            "write" => Ok(TWrite),
            "pid" => Ok(Pid),
            _ => Ok(Name),
        }
    }
}

#[derive(Debug)]
pub struct Prefix {
    pub and: Option<Box<And>>,
    pub regex_prefix: Option<(PrefixType, StringQuery)>,
    pub compare_prefix: Option<(PrefixType, NumericalQuery)>,
}

impl Prefix {
    pub fn process_regexes(
        &mut self, is_searching_whole_word: bool, is_ignoring_case: bool,
        is_searching_with_regex: bool,
    ) -> Result<()> {
        if let Some(and) = &mut self.and {
            return and.process_regexes(
                is_searching_whole_word,
                is_ignoring_case,
                is_searching_with_regex,
            );
        } else if let Some((prefix_type, query_content)) = &mut self.regex_prefix {
            if let StringQuery::Value(regex_string) = query_content {
                match prefix_type {
                    PrefixType::Pid | PrefixType::Name => {
                        let escaped_regex: String;
                        let final_regex_string = &format!(
                            "{}{}{}{}",
                            if is_searching_whole_word { "^" } else { "" },
                            if is_ignoring_case { "(?i)" } else { "" },
                            if !is_searching_with_regex {
                                escaped_regex = regex::escape(regex_string);
                                &escaped_regex
                            } else {
                                regex_string
                            },
                            if is_searching_whole_word { "$" } else { "" },
                        );

                        let taken_pwc = self.regex_prefix.take();
                        if let Some((taken_pt, _)) = taken_pwc {
                            self.regex_prefix = Some((
                                taken_pt,
                                StringQuery::Regex(regex::Regex::new(final_regex_string)?),
                            ));
                        }
                    }
                    _ => {}
                }
            }
        }

        Ok(())
    }

    pub fn check(&self, process: &ConvertedProcessData) -> bool {
        fn matches_condition(condition: &QueryComparison, lhs: f64, rhs: f64) -> bool {
            match condition {
                QueryComparison::Equal => (lhs - rhs).abs() < f64::EPSILON,
                QueryComparison::Less => lhs < rhs,
                QueryComparison::Greater => lhs > rhs,
                QueryComparison::LessOrEqual => lhs <= rhs,
                QueryComparison::GreaterOrEqual => lhs >= rhs,
            }
        }

        if let Some(and) = &self.and {
            and.check(process)
        } else if let Some((prefix_type, query_content)) = &self.regex_prefix {
            if let StringQuery::Regex(r) = query_content {
                match prefix_type {
                    PrefixType::Name => r.is_match(process.name.as_str()),
                    PrefixType::Pid => r.is_match(process.pid.to_string().as_str()),
                    _ => true,
                }
            } else {
                true
            }
        } else if let Some((prefix_type, numerical_query)) = &self.compare_prefix {
            match prefix_type {
                PrefixType::Cpu => matches_condition(
                    &numerical_query.condition,
                    process.cpu_usage,
                    numerical_query.value,
                ),
                PrefixType::Mem => matches_condition(
                    &numerical_query.condition,
                    process.mem_usage,
                    numerical_query.value,
                ),
                PrefixType::Rps => matches_condition(
                    &numerical_query.condition,
                    process.rps_f64,
                    numerical_query.value,
                ),
                PrefixType::Wps => matches_condition(
                    &numerical_query.condition,
                    process.wps_f64,
                    numerical_query.value,
                ),
                PrefixType::TRead => matches_condition(
                    &numerical_query.condition,
                    process.tr_f64,
                    numerical_query.value,
                ),
                PrefixType::TWrite => matches_condition(
                    &numerical_query.condition,
                    process.tw_f64,
                    numerical_query.value,
                ),
                _ => true,
            }
        } else {
            true
        }
    }
}

#[derive(Debug)]
pub enum QueryComparison {
    Equal,
    Less,
    Greater,
    LessOrEqual,
    GreaterOrEqual,
}

#[derive(Debug)]
pub enum StringQuery {
    Value(String),
    Regex(regex::Regex),
}

#[derive(Debug)]
pub struct NumericalQuery {
    pub condition: QueryComparison,
    pub value: f64,
}
