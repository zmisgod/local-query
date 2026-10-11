use serde_json::{json, Value};
use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
    sync::atomic::{AtomicUsize, Ordering},
};
static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Fixture {
    dir: PathBuf,
    path: PathBuf,
}
impl Fixture {
    fn new(name: &str, data: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "lselect-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        fs::write(&path, data).unwrap();
        Self { dir, path }
    }
    fn run(&self, query: &str, options: &[&str]) -> Output {
        let path = self.path.to_str().unwrap().replace('\'', "''");
        Command::new(env!("CARGO_BIN_EXE_lselect"))
            .args(options)
            .arg(query.replace("@file", &format!("'{path}'")).replace(
                "@folder",
                &format!("'{}'", self.dir.to_str().unwrap().replace('\'', "''")),
            ))
            .output()
            .unwrap()
    }
    fn query(&self, query: &str, options: &[&str]) -> Value {
        let result = self.run(query, options);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        serde_json::from_slice(&result.stdout).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn csv_filters_numbers_and_preserves_text() {
    let f = Fixture::new(
        "with spaces.csv",
        "name,id,age\n张三,002,20\n李四,2,9\n小王,3,unknown\n",
    );
    assert_eq!(
        f.query("* from @file where age >= 18", &[]),
        json!([{"name":"张三","id":"002","age":"20"}])
    );
    assert_eq!(
        f.query("name from @file where id = 2", &[]),
        json!([{"name":"张三"},{"name":"李四"}])
    );
    assert_eq!(
        f.query("name from @file where id = '002'", &[]),
        json!([{"name":"张三"}])
    );
}
#[test]
fn headerless_keeps_first_record() {
    let f = Fixture::new("data.csv", "first,2,x\nsecond,3,y\n");
    assert_eq!(
        f.query(
            "column1, column3 from @file where column2 = 2",
            &["--no-header"]
        ),
        json!([{"column1":"first","column3":"x"}])
    );
}
#[test]
fn tsv_and_quoted_csv_records() {
    let tsv = Fixture::new("data.tsv", "name\tage\n张三\t20\n");
    assert_eq!(tsv.query("name from @file", &[]), json!([{"name":"张三"}]));
    let csv = Fixture::new("data.csv", "name,note\n\"a,b\",\"line1\nline2\"\n");
    assert_eq!(
        csv.query("* from @file", &[]),
        json!([{"name":"a,b","note":"line1\nline2"}])
    );
}
#[test]
fn json_nested_fields_types_and_missing_values() {
    let f = Fixture::new(
        "users.json",
        r#"[{"name":"张三","age":20,"user":{"city":"上海"},"active":true},{"name":"李四","age":"20"},{"name":"小王","age":15,"user":null}]"#,
    );
    assert_eq!(
        f.query(
            "SELECT name, user.city AS city, active from @file where age >= 18",
            &[]
        ),
        json!([{"name":"张三","city":"上海","active":true}])
    );
    assert_eq!(
        f.query("name from @file where user.city IS NULL", &[]),
        json!([{"name":"李四"},{"name":"小王"}])
    );
    assert_eq!(
        f.query("name from @file where age = '20'", &[]),
        json!([{"name":"李四"}])
    );
}
#[test]
fn jsonl_and_double_encoded_objects() {
    let f = Fixture::new("events.txt", "{\"n\":1}\n\n\"{\\\"n\\\":2}\"\n{\"n\":3}\n");
    assert_eq!(
        f.query("* from @file where n >= 2 limit 1", &["--format", "jsonl"]),
        json!([{"n":2}])
    );
    let a = Fixture::new("encoded.json", r#"["{\"n\":2}"]"#);
    assert_eq!(a.query("* from @file", &[]), json!([{"n":2}]));
}
#[test]
fn logical_precedence_null_and_negative_numbers() {
    let f = Fixture::new("data.json", r#"[{"n":-2},{"n":1},{"n":3},{"n":null}]"#);
    assert_eq!(
        f.query("n from @file where (n < 0 OR n >= 3) AND NOT (n = 3)", &[]),
        json!([{"n":-2}])
    );
    assert_eq!(
        f.query("n from @file where n = -2 OR (n IS NULL AND true)", &[]),
        json!([{"n":-2},{"n":null}])
    );
    assert_eq!(f.query("n from @file where NOT (n = NULL)", &[]), json!([]));
}
#[test]
fn limit_stops_before_bad_unread_records() {
    for (name, text) in [
        ("data.json", "[{\"n\":1}, BROKEN]"),
        ("data.jsonl", "{\"n\":1}\nBROKEN\n"),
        ("data.csv", "n\n1\n2,3\n"),
    ] {
        let f = Fixture::new(name, text);
        assert_eq!(
            f.query("* from @file limit 1", &[])
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert!(!f.run("* from @file", &[]).status.success());
        assert_eq!(f.query("* from @file limit 0", &[]), json!([]));
    }
}
#[test]
fn precise_large_numbers() {
    let f = Fixture::new(
        "data.json",
        "[{\"n\":9007199254740992},{\"n\":9007199254740993}]",
    );
    assert_eq!(
        f.query("n from @file where n = 9007199254740993", &[]),
        json!([{"n":9007199254740993u64}])
    );
    let csv = Fixture::new("data.csv", "n\n9007199254740992\n9007199254740993\n");
    assert_eq!(
        csv.query("n from @file where n = 9007199254740993", &[]),
        json!([{"n":"9007199254740993"}])
    );
}
#[test]
fn paths_identifiers_and_literals_with_from() {
    let f = Fixture::new(
        "a' from b.json",
        r#"[{"from":"from x","user.city":"上海"}]"#,
    );
    assert_eq!(
        f.query(
            "\"from\", \"user.city\" from @file where \"from\" = 'from x'",
            &[]
        ),
        json!([{"from":"from x","user.city":"上海"}])
    );
}
#[test]
fn bare_relative_path_and_optional_prefix() {
    let f = Fixture::new("data.csv", "n\n2\n");
    let out = Command::new(env!("CARGO_BIN_EXE_lselect"))
        .current_dir(&f.dir)
        .arg("lselect * from ./data.csv where n = 2;")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&out.stdout).unwrap(),
        json!([{"n":"2"}])
    );
}
#[test]
fn unsupported_sql_is_rejected_even_for_empty_input() {
    let f = Fixture::new("empty.json", "[]");
    for query in [
        "unknown_function(n) from @file",
        "* from @file ORDER BY count(*)",
        "DISTINCT ON (n) n from @file",
        "* from @file LIMIT -1",
        "* from @file GROUP BY n",
        "* from @file; SELECT 1",
        "* from @file JOIN other ON true",
        "* from @file where n LIKE 'x'",
        "* from @file OFFSET 1",
    ] {
        let out = f.run(query, &[]);
        assert!(!out.status.success(), "Unexpected success: {query}");
        assert!(
            out.stdout.is_empty(),
            "Validation must happen before output: {query}"
        );
    }
}
#[test]
fn errors_include_record_context() {
    let f = Fixture::new("bad.jsonl", "{\"n\":1}\n{bad}\n");
    let out = f.run("* from @file", &[]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("JSONL line 2"));
    let f = Fixture::new("bad.json", "[{} , 42]");
    let out = f.run("* from @file", &[]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("JSON array element 2"));
}
#[test]
fn invalid_shapes_headers_and_trailing_data_fail() {
    for (name, data) in [
        ("object.json", "{}"),
        ("trailing.json", "[] garbage"),
        ("scalar.jsonl", "42\n"),
        ("duplicate.csv", "n,n\n1,2\n"),
        ("empty-header.csv", ",n\n1,2\n"),
        ("uneven.csv", "a,b\n1\n"),
    ] {
        let f = Fixture::new(name, data);
        assert!(!f.run("* from @file", &[]).status.success(), "{name}");
    }
}
#[test]
fn empty_inputs_and_duplicate_projection() {
    for (name, data) in [
        ("empty.json", "[]"),
        ("empty.jsonl", ""),
        ("empty.csv", "n\n"),
    ] {
        assert_eq!(
            Fixture::new(name, data).query("* from @file", &[]),
            json!([])
        );
    }
    let f = Fixture::new("data.json", "[{\"n\":1}]");
    assert!(!f.run("n, n from @file", &[]).status.success());
}
#[test]
fn cli_errors_and_help() {
    let help = Command::new(env!("CARGO_BIN_EXE_lselect"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(help.status.success());
    let f = Fixture::new("data.json", "[]");
    assert!(!f.run("* from @file", &["--no-header"]).status.success());
    assert!(!f
        .run("* from @file", &["--format", "excel"])
        .status
        .success());
    assert!(!f.run("* from @file", &["--unknown"]).status.success());
}

#[test]
fn unicode_identifiers_do_not_confuse_from_detection() {
    let f = Fixture::new("中文.json", r#"[{"来源from":"上海","姓名":"张三"}]"#);
    assert_eq!(
        f.query("来源from, 姓名 from @file where 姓名 = '张三'", &[]),
        json!([{"来源from":"上海","姓名":"张三"}])
    );
}

#[test]
fn order_multiple_keys_alias_and_limit_after_sort() {
    let f = Fixture::new(
        "data.json",
        r#"[{"name":"a","age":20,"city":"B"},{"name":"b","age":9,"city":"A"},{"name":"c","age":30,"city":"A"},{"name":"d","age":30,"city":"A"}]"#,
    );
    assert_eq!(f.query("name AS who, age from @file where age >= 10 order by city asc, age desc, who desc limit 2", &[]), json!([{"who":"d","age":30},{"who":"c","age":30}]));
    assert_eq!(
        f.query("name from @file order by age", &[]),
        json!([{"name":"b"},{"name":"a"},{"name":"c"},{"name":"d"}])
    );
}
#[test]
fn order_nulls_and_csv_numeric_order() {
    let f = Fixture::new("data.json", r#"[{"n":2},{"n":null},{"n":10},{}]"#);
    assert_eq!(
        f.query("n from @file order by n", &[]),
        json!([{"n":2},{"n":10},{"n":null},{"n":null}])
    );
    assert_eq!(
        f.query("n from @file order by n desc nulls last", &[]),
        json!([{"n":10},{"n":2},{"n":null},{"n":null}])
    );
    assert_eq!(
        f.query("n from @file order by n asc nulls first limit 1", &[]),
        json!([{"n":null}])
    );
    let csv = Fixture::new("data.csv", "n\n10\n2\n002\nword\n");
    assert_eq!(
        csv.query("n from @file order by n", &[]),
        json!([{"n":"2"},{"n":"002"},{"n":"10"},{"n":"word"}])
    );
}
#[test]
fn group_having_aggregates_aliases_and_where() {
    let f = Fixture::new("data.jsonl", "{\"city\":\"上海\",\"amount\":10}\n{\"city\":\"北京\",\"amount\":50}\n{\"city\":\"上海\",\"amount\":30}\n{\"city\":\"上海\",\"amount\":-1}\n");
    assert_eq!(f.query("SELECT city, COUNT(*) AS n, SUM(amount) AS total, AVG(amount) AS avg, MIN(amount) AS lo, MAX(amount) AS hi FROM @file WHERE amount > 0 GROUP BY city HAVING n >= 2 AND SUM(amount) > 30 ORDER BY total DESC LIMIT 1", &[]), json!([{"city":"上海","n":2,"total":40,"avg":20,"lo":10,"hi":30}]));
}
#[test]
fn group_multiple_nested_keys_and_null_group() {
    let f = Fixture::new(
        "data.json",
        r#"[{"user":{"city":"A"},"kind":1},{"user":{"city":"A"},"kind":1},{"user":{"city":"A"},"kind":2},{"kind":1},{"user":{"city":null},"kind":1}]"#,
    );
    assert_eq!(f.query("user.city AS city, kind, COUNT(*) AS n from @file group by user.city, kind order by city, kind", &[]), json!([{"city":"A","kind":1,"n":2},{"city":"A","kind":2,"n":1},{"city":null,"kind":1,"n":2}]));
}
#[test]
fn global_aggregates_and_empty_input_semantics() {
    let f = Fixture::new("data.json", r#"[{"n":2},{"n":null},{"n":4},{}]"#);
    assert_eq!(
        f.query(
            "COUNT(*) AS rows, COUNT(n) AS present, SUM(n) AS total, AVG(n) AS avg from @file",
            &[]
        ),
        json!([{"rows":4,"present":2,"total":6,"avg":3}])
    );
    assert_eq!(
        f.query("SUM(n) AS total from @file having count(*) = 4", &[]),
        json!([{"total":6}])
    );
    assert_eq!(f.query("COUNT(*) AS n, SUM(n) AS total, AVG(n) AS avg, MIN(n) AS lo, MAX(n) AS hi from @file where false", &[]), json!([{"n":0,"total":null,"avg":null,"lo":null,"hi":null}]));
    assert_eq!(
        f.query(
            "n, COUNT(*) AS count from @file where false group by n",
            &[]
        ),
        json!([])
    );
    assert_eq!(
        f.query("COUNT(*) AS n from @file having count(*) > 9", &[]),
        json!([])
    );
    assert_eq!(
        Fixture::new("empty.json", "[]").query("COUNT(*) AS n from @file", &[]),
        json!([{"n":0}])
    );
}
#[test]
fn distinct_projection_and_aggregate_distinct() {
    let f = Fixture::new(
        "data.json",
        r#"[{"n":1},{"n":1.0},{"n":1e0},{"n":2},{"n":null},{}]"#,
    );
    assert_eq!(
        f.query(
            "DISTINCT n from @file order by n desc nulls last limit 2",
            &[]
        ),
        json!([{"n":2},{"n":1}])
    );
    assert_eq!(
        f.query(
            "COUNT(DISTINCT n) AS n, SUM(DISTINCT n) AS total, AVG(DISTINCT n) AS avg from @file",
            &[]
        ),
        json!([{"n":2,"total":3,"avg":1.5}])
    );
    assert_eq!(
        f.query("n, COUNT(*) AS count from @file group by n order by n", &[]),
        json!([{"n":1,"count":3},{"n":2,"count":1},{"n":null,"count":2}])
    );
}
#[test]
fn arithmetic_projection_group_expression_and_ordinals() {
    let f = Fixture::new("data.json", r#"[{"n":2},{"n":1},{"n":2}]"#);
    assert_eq!(
        f.query(
            "n + 1 AS n, COUNT(*) AS count from @file group by 1 order by 1 desc",
            &[]
        ),
        json!([{"n":3,"count":2},{"n":2,"count":1}])
    );
    assert_eq!(f.query("n % 2 AS parity, SUM(n * 2) + 1 AS score from @file group by parity having score > 3 order by score", &[]), json!([{"parity":0,"score":9}]));
    assert_eq!(
        f.query(
            "n * 2 AS doubled, (n + 1) / 2 AS half from @file order by 2, 1 limit 1",
            &[]
        ),
        json!([{"doubled":2,"half":1}])
    );
    assert_eq!(
        f.query(
            "n, COUNT(*) AS count from @file group by (n) order by n",
            &[]
        ),
        json!([{"n":1,"count":1},{"n":2,"count":2}])
    );
}
#[test]
fn csv_aggregation_and_headerless_grouping() {
    let f = Fixture::new("data.csv", "A,10\nB,20\nA,2\n");
    assert_eq!(f.query("column1, SUM(column2) AS total, MIN(column2) AS lo, MAX(column2) AS hi from @file group by column1 order by total desc", &["--no-header"]), json!([{"column1":"B","total":20,"lo":"20","hi":"20"},{"column1":"A","total":12,"lo":"2","hi":"10"}]));
    assert_eq!(
        f.query(
            "-column2 AS negative from @file order by negative limit 1",
            &["--no-header", "--"]
        ),
        json!([{"negative":-20}])
    );
}
#[test]
fn aggregate_and_group_validation_even_for_empty_files() {
    let f = Fixture::new("empty.json", "[]");
    for query in [
        "n, COUNT(*) from @file",
        "n from @file group by n + 1",
        "* from @file group by n",
        "n from @file where COUNT(*) > 0",
        "n from @file group by COUNT(*)",
        "SUM(COUNT(n)) from @file",
        "SUM(*) from @file",
        "COUNT(DISTINCT *) from @file",
        "COUNT(n, x) from @file",
        "COUNT(*) OVER () from @file",
        "n from @file group by n having x > 1",
        "n from @file group by n order by x",
        "n from @file order by 0",
        "n from @file order by 2",
        "DISTINCT n from @file order by x",
        "n AS x, n AS x from @file",
    ] {
        let out = f.run(query, &[]);
        assert!(!out.status.success(), "Unexpected success: {query}");
        assert!(
            out.stdout.is_empty(),
            "Validation occurred too late: {query}"
        );
    }
}
#[test]
fn buffered_queries_read_past_limit_and_report_errors() {
    let f = Fixture::new("bad.jsonl", "{\"n\":1}\nBAD\n");
    for query in [
        "* from @file order by n limit 1",
        "COUNT(*) from @file limit 1",
        "DISTINCT n from @file limit 1",
    ] {
        let out = f.run(query, &[]);
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains("JSONL line 2"));
    }
    let f = Fixture::new("bad.csv", "n\n1\nword\n");
    let out = f.run("SUM(n) from @file", &[]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("numeric CSV"));
    let f = Fixture::new("data.json", "[{\"n\":1}]");
    assert!(!f.run("n / 0 from @file", &[]).status.success());
}
#[test]
fn sorting_and_sums_preserve_decimal_precision() {
    let f = Fixture::new(
        "data.json",
        "[{\"n\":9007199254740993},{\"n\":9007199254740992}]",
    );
    assert_eq!(
        f.query("n from @file order by n limit 1", &[]),
        json!([{"n":9007199254740992u64}])
    );
    let f = Fixture::new("data.json", "[{\"n\":0.1},{\"n\":0.2}]");
    assert_eq!(
        f.query("SUM(n) AS total from @file", &[]),
        json!([{"total":0.3}])
    );
}

#[test]
fn folder_global_distinct_group_order_and_limit() {
    let f = Fixture::new(
        "b.jsonl",
        "{\"level\":\"info\",\"n\":2}\n{\"level\":\"error\",\"n\":3}\n",
    );
    fs::write(
        f.dir.join("a.json"),
        r#"[{"level":"info","n":1},{"level":"debug","n":4}]"#,
    )
    .unwrap();
    assert_eq!(
        f.query(
            "DISTINCT level FROM FOLDER @folder ORDER BY level LIMIT 2",
            &[]
        ),
        json!([{"level":"debug"},{"level":"error"}])
    );
    assert_eq!(f.query("level, COUNT(*) AS count, SUM(n) AS total from folder @folder group by level having count > 1 order by total desc", &[]), json!([{"level":"info","count":2,"total":3}]));
    assert_eq!(
        f.query("n from folder @folder limit 3", &[]),
        json!([{"n":1},{"n":4},{"n":2}])
    );
}
#[test]
fn folder_explicit_format_logs_and_limit_stop_across_files() {
    let f = Fixture::new("a.log", "{\"level\":\"info\"}\n");
    fs::write(f.dir.join("b.log"), "{\"level\":\"debug\"}\nBROKEN\n").unwrap();
    assert_eq!(
        f.query("level from folder @folder limit 2", &["--format", "jsonl"]),
        json!([{"level":"info"},{"level":"debug"}])
    );
    let out = f.run(
        "level from folder @folder order by level limit 1",
        &["--format", "jsonl"],
    );
    assert!(!out.status.success());
    let error = String::from_utf8_lossy(&out.stderr);
    assert!(error.contains("b.log") && error.contains("JSONL line 2"));
    assert_eq!(f.query("* from folder @folder", &[]), json!([]));
}
#[test]
fn folder_recursive_hidden_and_unknown_files() {
    let f = Fixture::new("a.jsonl", "{\"n\":1}\n");
    fs::create_dir(f.dir.join("sub")).unwrap();
    fs::write(f.dir.join("sub/b.jsonl"), "{\"n\":2}\n").unwrap();
    fs::write(f.dir.join(".hidden.jsonl"), "BAD").unwrap();
    fs::write(f.dir.join("readme.txt"), "not data").unwrap();
    fs::create_dir(f.dir.join(".hidden")).unwrap();
    fs::write(f.dir.join(".hidden/b.jsonl"), "BAD").unwrap();
    assert_eq!(
        f.query("COUNT(*) AS n from folder @folder", &[]),
        json!([{"n":1}])
    );
    assert_eq!(
        f.query("COUNT(*) AS n from folder @folder", &["--recursive"]),
        json!([{"n":2}])
    );
    assert!(!f.run("* from @file", &["--recursive"]).status.success());
}
#[test]
fn folder_csv_independent_headers_and_missing_fields() {
    let f = Fixture::new("a.csv", "name,age\na,10\n");
    fs::write(f.dir.join("b.csv"), "age,name,city\n20,b,X\n").unwrap();
    assert_eq!(
        f.query("name, age, city from folder @folder order by age desc", &[]),
        json!([{"name":"b","age":"20","city":"X"},{"name":"a","age":"10","city":null}])
    );
    assert_eq!(
        f.query("SUM(age) AS total from folder @folder", &[]),
        json!([{"total":30}])
    );
    fs::write(&f.path, "a,10\n").unwrap();
    fs::write(f.dir.join("b.csv"), "b,20\n").unwrap();
    assert_eq!(
        f.query(
            "SUM(column2) AS total from folder @folder",
            &["--no-header"]
        ),
        json!([{"total":30}])
    );
}
#[test]
fn folder_quoted_paths_and_file_named_folder_remain_distinct() {
    let f = Fixture::new("folder", "{\"n\":1}\n");
    assert_eq!(
        f.query("n from @file", &["--format", "jsonl"]),
        json!([{"n":1}])
    );
    let nested = f.dir.join("a' 空 格");
    fs::create_dir(&nested).unwrap();
    fs::write(nested.join("data.json"), "[{\"n\":2}]").unwrap();
    let sql = format!("n FrOm FoLdEr \"{}\"", nested.display());
    assert_eq!(f.query(&sql, &[]), json!([{"n":2}]));
    assert!(!f.run("* from folder", &[]).status.success());
    assert!(!f.run("* from folder unquoted", &[]).status.success());
    assert!(!f.run("* from folder @file", &[]).status.success());
    assert!(!f.run("* from @folder", &[]).status.success());
}
#[test]
fn empty_folder_and_incompatible_formats() {
    let f = Fixture::new("ignore.txt", "ignored");
    assert_eq!(f.query("* from folder @folder", &[]), json!([]));
    assert_eq!(
        f.query("COUNT(*) AS n from folder @folder", &[]),
        json!([{"n":0}])
    );
    fs::write(f.dir.join("a.csv"), "n\n1\n").unwrap();
    fs::write(f.dir.join("b.json"), "[{\"n\":2}]").unwrap();
    let out = f.run("* from folder @folder", &[]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("mixes CSV/TSV"));
}
#[cfg(unix)]
#[test]
fn recursive_folder_skips_symlinks() {
    let f = Fixture::new("a.json", "[{\"n\":1}]");
    std::os::unix::fs::symlink(&f.dir, f.dir.join("cycle")).unwrap();
    std::os::unix::fs::symlink(&f.path, f.dir.join("alias.json")).unwrap();
    assert_eq!(
        f.query("COUNT(*) AS n from folder @folder", &["--recursive"]),
        json!([{"n":1}])
    );
}
