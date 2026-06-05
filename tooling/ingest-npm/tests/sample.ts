// Sample module for exercising nl-ingest-ts. Not part of Novae Linguae itself.

/** A plain exported function. */
export function add(a: number, b: number): number {
  return a + b;
}

// Generic + array types; a nested arrow-type parameter (its comma must not split params).
export function map<T, U>(xs: T[], f: (x: T) => U): U[] {
  return xs.map(f);
}

// async, Promise return, generic map parameter.
export async function fetchJson(url: string, headers: Map<string, string>): Promise<unknown> {
  return {};
}

// Arrow const with explicit return type.
export const toUpper = (s: string): string => s.toUpperCase();

// Arrow const, generic, optional + default + rest params (arity counts 3 declared params).
export const pick = <T>(obj: T, key?: string, ...rest: string[]): unknown => obj;

// const = function expression (anonymous); binding name `negate` wins.
export const negate = function (n: number): number {
  return -n;
};

// Single bare-identifier arrow parameter, no annotations.
export const identity = x => x;

// Ambient .d.ts-style declaration (no body, semicolon-terminated).
export declare function parseConfig(text: string): Record<string, number>;

// default export, anonymous.
export default function (n: number): boolean {
  return n > 0;
}

// Not exported — must be skipped.
function internalHelper(x: number): number {
  return x * 2;
}

// Non-function const export — must be skipped.
export const VERSION = "1.0.0";
