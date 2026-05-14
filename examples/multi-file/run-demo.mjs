// Smoke test for the multi-file build. Run after `psx build` + tsc:
//   node examples/multi-file/run-demo.mjs
import { App } from "./dist-js/Main.js";
import { User } from "./dist-js/Models/User.js";

const user = new User("u2", "grace@example.com");
const result = App.greet(user);
console.log("greet result:", result);
