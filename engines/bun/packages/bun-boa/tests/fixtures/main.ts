import { greet, type Greeting } from "./greet.ts";
import data from "./data.json" with { type: "json" };

interface Box<T> {
  value: T;
}
const box: Box<Greeting> = { value: greet("bun") };
console.log(box.value);
console.log(`json ${data.name} ${data.list.length}`);

const out = `${import.meta.dir}/out.txt`;
const written = await Bun.write(out, "written by Bun.write");
console.log(`write ${written} ${await Bun.file(out).text()} ${await Bun.file(out).exists()}`);
console.log(`json-file ${(await Bun.file(`${import.meta.dir}/data.json`).json()).name}`);
console.log(`missing ${await Bun.file(`${import.meta.dir}/missing.txt`).exists()}`);

await new Promise<void>(resolve => setTimeout(resolve, 5));
console.log(`timer done, version ${typeof Bun.version}, env ${Bun.env.BUN_BOA_TEST === process.env.BUN_BOA_TEST}`);
console.log(`argv ${process.argv.slice(2).join(",")}`);
console.log(`main ${import.meta.main}`);
process.exitCode = 3;
