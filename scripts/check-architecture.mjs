// Keep the public check command independent from package managers and Cargo.
// Importing the suite registers its node:test cases in this process.
import "./architecture.test.mjs";
import "./tauri-security.test.mjs";
