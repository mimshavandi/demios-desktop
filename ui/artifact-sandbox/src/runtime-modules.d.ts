// Built in-frame runtimes, inlined as text by scripts/build.mjs.
declare module "runtime:*" {
  const code: string;
  export default code;
}
