import { assertEquals } from "@std/assert";

// Test the regex pattern used in build.ts to patch __require
const PATCH_REGEX =
  /throw (Error\('Dynamic require of "' \+ (\w+) \+ '" is not supported'\))/;

Deno.test("build patch regex matches esbuild CJS shim with variable 'x'", () => {
  const input =
    `throw Error('Dynamic require of "' + x + '" is not supported')`;
  const match = PATCH_REGEX.exec(input);
  assertEquals(match !== null, true);
  assertEquals(match![2], "x");
});

Deno.test("build patch regex matches esbuild CJS shim with minified variable 'x2'", () => {
  const input =
    `throw Error('Dynamic require of "' + x2 + '" is not supported')`;
  const match = PATCH_REGEX.exec(input);
  assertEquals(match !== null, true);
  assertEquals(match![2], "x2");
});

Deno.test("build patch regex matches esbuild CJS shim with variable 'a3'", () => {
  const input =
    `throw Error('Dynamic require of "' + a3 + '" is not supported')`;
  const match = PATCH_REGEX.exec(input);
  assertEquals(match !== null, true);
  assertEquals(match![2], "a3");
});

Deno.test("build patch produces correct replacement", () => {
  const input = `if (typeof require !== "undefined")
    return require.apply(this, arguments);
  throw Error('Dynamic require of "' + x2 + '" is not supported');`;

  const result = input.replace(
    PATCH_REGEX,
    (_match, errExpr, varName) =>
      `if(${varName}==="buffer")return globalThis.__buffer_polyfill;throw ${errExpr}`,
  );

  assertEquals(
    result.includes('if(x2==="buffer")return globalThis.__buffer_polyfill;'),
    true,
  );
  assertEquals(result.includes("throw Error('Dynamic require of \"'"), true);
});

Deno.test("build patch guard detects when regex doesn't match", () => {
  const input = "some completely different esbuild output";
  const before = input;
  const after = input.replace(PATCH_REGEX, "replaced");

  // Simulates the guard in build.ts
  assertEquals(
    before === after,
    true,
    "No match means before === after, which should trigger build failure",
  );
});
