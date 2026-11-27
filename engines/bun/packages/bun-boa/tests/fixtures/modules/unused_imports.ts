// A name that is a type only, and a name imported twice: TypeScript drops the imports it does not use, so neither is
// an error (test/js/web/fetch/headers.test.ts imports `bunEnv` and `bunExe` twice).
import { NotAnExport, version } from "bun";
import { mapped } from "./lib/mapped.ts";
import { mapped } from "./lib/mapped.ts";

const kind: NotAnExport | string = typeof version;
console.log(kind, mapped);
