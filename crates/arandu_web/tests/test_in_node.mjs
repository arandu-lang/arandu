import { readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const __dirname = dirname(fileURLToPath(import.meta.url));
const wasmPath = join(__dirname, '../../../target/wasm32-unknown-unknown/release/arandu_web.wasm');
const wasmBytes = await readFile(wasmPath);

const { instance } = await WebAssembly.instantiate(wasmBytes, {});
const memory = instance.exports.memory;
const { arandu_alloc, arandu_free, arandu_compile, arandu_free_response } = instance.exports;

function compile(source) {
  const enc = new TextEncoder();
  const sourceBytes = enc.encode(source);
  const ptr = arandu_alloc(sourceBytes.length);
  new Uint8Array(memory.buffer, ptr, sourceBytes.length).set(sourceBytes);

  const respPtr = arandu_compile(ptr, sourceBytes.length);
  const view = new DataView(memory.buffer, respPtr, 20);
  const success = view.getUint32(0, true) === 1;
  const wasmPtr = view.getUint32(4, true);
  const wasmLen = view.getUint32(8, true);
  const jsonPtr = view.getUint32(12, true);
  const jsonLen = view.getUint32(16, true);

  let outputWasm = null;
  if (success && wasmPtr !== 0 && wasmLen > 0) {
    outputWasm = new Uint8Array(memory.buffer, wasmPtr, wasmLen).slice();
  }

  let diagnostics = [];
  if (jsonPtr !== 0 && jsonLen > 0) {
    const jsonStr = new TextDecoder().decode(new Uint8Array(memory.buffer, jsonPtr, jsonLen));
    diagnostics = JSON.parse(jsonStr);
  }

  arandu_free_response(respPtr);
  arandu_free(ptr, sourceBytes.length);
  return { success, outputWasm, diagnostics };
}

console.log('Testing in-browser compiler WebAssembly...');
const program = `
extern "env" {
    func print_str(msg: str): void
}

public func main(): i32 {
    unsafe {
        print_str("Olá do Arandu via WebAssembly no Navegador!")
    }
    return 100
}
`;

const res = compile(program);
console.log('Compilation success:', res.success);
console.log('Emitted wasm bytes:', res.outputWasm?.length);
console.log('Diagnostics:', res.diagnostics);

if (!res.success || !res.outputWasm) {
  console.error('Compilation failed!');
  process.exit(1);
}

const mod = new WebAssembly.Module(res.outputWasm);
console.log('Guest module imports:', WebAssembly.Module.imports(mod));
console.log('Guest module exports:', WebAssembly.Module.exports(mod));

// Now instantiate and RUN the compiled program!
let guestMemory;
let printedText = '';
const guestImports = {
  env: {
    print_str: (ptr, len) => {
      const bytes = new Uint8Array(guestMemory.buffer, ptr, len);
      printedText += new TextDecoder().decode(bytes) + '\n';
    }
  }
};

const guestModule = await WebAssembly.instantiate(res.outputWasm, guestImports);
guestMemory = guestModule.instance.exports.memory;
const retCode = guestModule.instance.exports.main();
console.log('Guest execution return code:', retCode);
console.log('Guest printed output:');
process.stdout.write(printedText);

if (retCode === 100 && printedText.includes('Olá do Arandu')) {
  console.log('\n>>> SUCCESS! In-browser compilation and execution with host I/O works 100%! <<<');
} else {
  console.error('Execution assertion failed: expected 100, got', retCode);
  process.exit(1);
}
