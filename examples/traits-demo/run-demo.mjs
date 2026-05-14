// Smoke test for the traits multi-file build:
//   psx build && tsc && node run-demo.mjs
import { makeMessage, makeAdminMessage } from "./dist-js/Main.js";

console.log("greeting:", makeMessage());
console.log("admin:   ", makeAdminMessage());
