#!/usr/bin/env node
// ts_enrich.js — the TypeScript-compiler backend for the nl-ingest-ts toolchain seam.
//
// Reads a TypeScript source on stdin and writes an ENRICHED source on stdout: the compiler's
// emitted `.d.ts` declarations, in which every exported function carries a fully-resolved /
// inferred signature (return types the scanner could not recover are now explicit, type aliases
// expanded). The nl-ingest-ts string scanner already understands `export declare function …;`
// ambient declarations, so it parses these high-fidelity signatures directly.
//
// Requires the `typescript` package to be resolvable (global, local, or via NODE_PATH). If it is
// not, `require('typescript')` throws and this exits non-zero — the Python side then falls back to
// the original source, so the toolchain path is never worse than the scanner. JSDoc comments
// (including `@example`, used by --v2) are preserved in the declaration output.
//
// Trade-off, by design: `.d.ts` declarations have no bodies, so in toolchain mode body-expression
// ASTs and token-scanned effects fall back — the gain is signature fidelity, which is the point.

"use strict";

let ts;
try {
  ts = require("typescript");
} catch (e) {
  process.stderr.write("ts_enrich: the 'typescript' package is not resolvable\n");
  process.exit(2);
}

let src = "";
process.stdin.setEncoding("utf8");
process.stdin.on("data", (d) => (src += d));
process.stdin.on("end", () => {
  const fileName = "input.ts";
  const options = {
    declaration: true,
    emitDeclarationOnly: true,
    removeComments: false, // keep JSDoc (@example) for --v2 example mining
    noEmitOnError: false,
    skipLibCheck: true,
    strict: false,
    target: ts.ScriptTarget.Latest,
    moduleResolution: ts.ModuleResolutionKind.NodeJs,
  };

  const host = ts.createCompilerHost(options);
  const origGetSourceFile = host.getSourceFile.bind(host);
  const origReadFile = host.readFile.bind(host);
  const origFileExists = host.fileExists.bind(host);

  let dts = "";
  host.getSourceFile = (name, languageVersion, onError, shouldCreate) => {
    if (name === fileName) return ts.createSourceFile(name, src, languageVersion, true);
    return origGetSourceFile(name, languageVersion, onError, shouldCreate);
  };
  host.readFile = (name) => (name === fileName ? src : origReadFile(name));
  host.fileExists = (name) => (name === fileName ? true : origFileExists(name));
  host.writeFile = (name, contents) => {
    if (name.endsWith(".d.ts")) dts = contents;
  };

  try {
    const program = ts.createProgram([fileName], options, host);
    program.emit();
  } catch (e) {
    process.stderr.write("ts_enrich: " + (e && e.message ? e.message : String(e)) + "\n");
    process.exit(3);
  }

  // Empty declaration output (no exports) -> signal fallback to the original source.
  if (!dts.trim()) process.exit(4);
  process.stdout.write(dts);
});
