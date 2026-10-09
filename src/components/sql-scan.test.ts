import { describe, expect, it } from "vitest";
import { scan, visibleAliases } from "./SqlEditor";

const KEYWORDS = new Set([
  "SELECT", "FROM", "WHERE", "JOIN", "LEFT", "ON", "AS", "AND", "GROUP", "BY",
  "ORDER", "INNER", "UNION", "WITH",
]);

/** Column lookup shaped the way the editor builds it from the catalog. */
const catalog = new Map([
  ["users", [{ column: "id", dataType: "int" }, { column: "name", dataType: "varchar" }]],
  ["orders", [{ column: "id", dataType: "int" }, { column: "user_id", dataType: "int" }]],
]);

/** Put the caret at the end of the text, or right after `marker`. */
function aliasesAt(sql: string) {
  const caret = sql.length;
  const { tokens } = scan(sql, KEYWORDS);
  return visibleAliases(tokens, caret, catalog);
}

describe("scan", () => {
  it("keeps every character so the highlight layer lines up", () => {
    const sql = "SELECT 1 -- 注释\nFROM t /* 块 */ WHERE x = 'a''b'";
    const { tokens } = scan(sql, KEYWORDS);
    expect(tokens.map((token) => token.text).join("")).toBe(sql);
  });

  it("classifies keywords, strings, comments and numbers", () => {
    const { tokens } = scan("SELECT 1, 'x' -- c", KEYWORDS);
    const byType = (type: string) => tokens.filter((t) => t.type === type).map((t) => t.text);
    expect(byType("keyword")).toContain("SELECT");
    expect(byType("number")).toContain("1");
    expect(byType("string")).toContain("'x'");
    expect(byType("comment")).toContain("-- c");
  });

  it("does not treat a keyword inside a string as a keyword", () => {
    const { tokens } = scan("SELECT 'FROM'", KEYWORDS);
    expect(tokens.filter((t) => t.type === "keyword").map((t) => t.text)).toEqual(["SELECT"]);
  });

  it("reports the structural mistakes it can see", () => {
    expect(scan("SELECT 'unclosed", KEYWORDS).issues).not.toHaveLength(0);
    expect(scan("SELECT /* open", KEYWORDS).issues).not.toHaveLength(0);
    expect(scan("SELECT (1", KEYWORDS).issues.join()).toContain("左括号");
    expect(scan("SELECT 1)", KEYWORDS).issues.join()).toContain("右括号");
    expect(scan("SELECT (1)", KEYWORDS).issues).toHaveLength(0);
  });

  it("tracks offsets", () => {
    const { tokens } = scan("SELECT 1", KEYWORDS);
    expect(tokens[0].start).toBe(0);
    expect(tokens[1].start).toBe(6);
  });
});

describe("alias resolution", () => {
  it("resolves a plain table alias", () => {
    const aliases = aliasesAt("SELECT * FROM users u WHERE u.");
    expect(aliases.get("u")?.map((c) => c.column)).toEqual(["id", "name"]);
  });

  it("resolves the table name itself when there is no alias", () => {
    const aliases = aliasesAt("SELECT * FROM users WHERE users.");
    expect(aliases.get("users")?.map((c) => c.column)).toEqual(["id", "name"]);
  });

  it("honours the AS form", () => {
    const aliases = aliasesAt("SELECT * FROM orders AS o WHERE o.");
    expect(aliases.get("o")?.map((c) => c.column)).toEqual(["id", "user_id"]);
  });

  it("resolves an alias introduced by JOIN", () => {
    const aliases = aliasesAt(
      "SELECT * FROM users u JOIN orders o ON o.user_id = u.id WHERE o.",
    );
    expect(aliases.get("o")?.map((c) => c.column)).toEqual(["id", "user_id"]);
    expect(aliases.get("u")?.map((c) => c.column)).toEqual(["id", "name"]);
  });

  it("sees an outer alias from inside a subquery", () => {
    // The correlated case the user asked about.
    const aliases = aliasesAt(
      "SELECT * FROM users u WHERE EXISTS (SELECT 1 FROM orders o WHERE o.user_id = u.",
    );
    expect(aliases.get("u")?.map((c) => c.column)).toEqual(["id", "name"]);
    expect(aliases.get("o")?.map((c) => c.column)).toEqual(["id", "user_id"]);
  });

  it("keeps a closed subquery's alias out of the enclosing query", () => {
    // `o` is out of scope once its subquery has ended.
    const aliases = aliasesAt(
      "SELECT * FROM users u WHERE id IN (SELECT user_id FROM orders o) AND u.",
    );
    expect(aliases.has("u")).toBe(true);
    expect(aliases.has("o")).toBe(false);
  });

  it("does not leak aliases between sibling subqueries", () => {
    const aliases = aliasesAt(
      "SELECT * FROM (SELECT id FROM users u) a, (SELECT id FROM orders o) b WHERE b.",
    );
    // Neither subquery's inner alias survives into the enclosing query.
    expect(aliases.has("u")).toBe(false);
    expect(aliases.has("o")).toBe(false);
    // The derived table aliases are what remain in scope.
    expect(aliases.get("a")?.map((c) => c.column)).toEqual(["id"]);
    expect(aliases.get("b")?.map((c) => c.column)).toEqual(["id"]);
  });

  it("reads a derived table's select list", () => {
    const aliases = aliasesAt("SELECT * FROM (SELECT id, name FROM users) AS x WHERE x.");
    expect(aliases.get("x")?.map((c) => c.column)).toEqual(["id", "name"]);
  });

  it("strips a table qualifier from a derived table's select list", () => {
    // `u.id` in the subquery becomes the column `id` of the derived table.
    const aliases = aliasesAt(
      "SELECT * FROM (SELECT u.id, u.name FROM users u) AS x WHERE x.",
    );
    expect(aliases.get("x")?.map((c) => c.column)).toEqual(["id", "name"]);
  });

  it("resolves a schema-qualified table", () => {
    // The catalog is keyed by table name, so `app.users` must fall back to it.
    const aliases = aliasesAt("SELECT * FROM app.users WHERE users.");
    expect(aliases.get("users")?.map((c) => c.column)).toEqual(["id", "name"]);
  });

  it("ignores aliases written inside strings and comments", () => {
    const aliases = aliasesAt("SELECT '-- FROM users u' FROM orders o WHERE o.");
    expect(aliases.has("u")).toBe(false);
    expect(aliases.get("o")?.map((c) => c.column)).toEqual(["id", "user_id"]);
  });

  it("does not mistake a following keyword for an alias", () => {
    const aliases = aliasesAt("SELECT * FROM users WHERE users.");
    expect(aliases.has("where")).toBe(false);
  });
});
