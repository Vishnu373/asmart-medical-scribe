import js from "@eslint/js";
import tseslint from "@typescript-eslint/eslint-plugin";
import tsparser from "@typescript-eslint/parser";
import reactHooks from "eslint-plugin-react-hooks";

export default [
  js.configs.recommended,
  {
    files: ["src/**/*.{ts,tsx}"],
    languageOptions: {
      parser: tsparser,
      parserOptions: {
        ecmaVersion: "latest",
        sourceType: "module",
        ecmaFeatures: { jsx: true },
      },
    },
    plugins: { "@typescript-eslint": tseslint, "react-hooks": reactHooks },
    rules: {
      ...tseslint.configs.recommended.rules,
      "@typescript-eslint/no-explicit-any": "error",
      // `tsc` already rejects undefined identifiers, so on typed sources this rule
      // only re-flags DOM builtins (document, setTimeout, HTMLElement…) as errors.
      "no-undef": "off",
      // The plugin ships ~28 React Compiler rules as well; only the two classic
      // hook rules are enabled — `exhaustive-deps` is what the in-code disable
      // comments across the views refer to.
      "react-hooks/rules-of-hooks": "error",
      "react-hooks/exhaustive-deps": "warn",
    },
  },
];
