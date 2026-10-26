# RFC 0028: Backend eBPF Nativo e Compilação Cooperativa com o Verificador (`arandu_backend_ebpf`)

- **Número da RFC:** 0028
- **Título:** Backend eBPF Nativo e Compilação Cooperativa com o Verificador (`arandu_backend_ebpf`)
- **Autor(es):** Bruno e Equipe do Compilador Arandu
- **Data de Início:** 2026-10-15
- **Status:** `Draft`
- **Área Principal:** `Backend` / `Kernel` / `Tooling` (`arandu_backend_ebpf`)
- **Documentos Relacionados:**
  - `docs/arandu-compiler-roadmap-v0.1.md`
  - `docs/rfcs/0007-semantic-memory-model.md`
  - `docs/rfcs/0013-deterministic-ctfe-and-comptime-metaprogramming.md`
  - `docs/rfcs/0017-lean-freestanding-core-architecture.md`
  - `docs/rfcs/0027-zero-bloat-binary-diet-and-on-demand-runtime.md`

> **Estado auditado em 2026-10-15:** Proposta exploratória (`Draft`), sem crate
> de backend implementado na árvore atual. A fila permanente e os critérios de
> priorização vivem exclusivamente no
> [roadmap mestre](../arandu-compiler-roadmap-v0.1.md). A verificação OSSA do
> compilador Arandu ajuda a restringir o subconjunto admitido e a preservar
> invariantes de ponteiros na geração de código, mas **não substitui** a
> aceitação pelo verificador in-kernel do Linux (`kernel/bpf/verifier.c`).

---

## 1. Resumo (Summary)

Esta RFC propõe a especificação técnica e o modelo arquitetural de um futuro **Backend eBPF Nativo (`arandu_backend_ebpf`)** para o compilador Arandu. O objetivo é permitir a escrita de programas de rede de baixo nível (XDP, TC), filtros de pacotes e instrumentação de observabilidade em um subconjunto estritamente verificado de Arandu, emitindo objetos ELF eBPF de 64 bits (`bpfel`/`bpfeb`) acompanhados de metadados BTF (*BPF Type Format*).

A proposta endereça o chamado **"Compiler Gap"** observado em toolchains baseadas em C e Rust/LLVM através de quatro mecanismos:
1. **Emissão Direta AMIR $\rightarrow$ ISA eBPF (`bpfel`/`bpfeb`)**: Tradução controlada do grafo SSA da AMIR para os registradores virtuais do eBPF ($R0$–$R10$), evitando transformações algébricas agressivas do LLVM que desfazem a proveniência de ponteiros exigida pelo verificador do kernel;
2. **Validação de Subconjunto e Orçamento de Pilha em Compilação**: Rejeição estática de alocação dinâmica de heap, chamadas indiretas arbitrárias, recursão e quadros de pilha que excedam o limite arquitetural de **512 bytes** do eBPF;
3. **Laços Limitados Cooperativos (`comptime for` e Iteradores BPF)**: Emissão de laços pequenos desenrolados via `comptime for` ou laços limitados com variável de indução monotônica explícita e iteradores abertos do kernel (`bpf_iter_num` em kernels $\ge 6.4$), prevenindo a explosão de estados do verificador;
4. **Rastreamento de Invalidação de Ponteiros de Pacote**: Modelagem estática da invalidação de fatias `PacketView` (`ctx->data` / `ctx->data_end`) após chamadas a helpers do kernel que realocam cabeçalhos de rede (como `bpf_xdp_adjust_head`).

---

## 2. Motivação (Motivation)

O eBPF (*Extended Berkeley Packet Filter*) consolidou-se como a tecnologia padrão para processamento de pacotes em linha de placa de rede (XDP), roteamento cloud-native (Cilium/Katran) e auditoria de segurança (Falco/Tetragon). Contudo, a compilação de linguagens de alto nível para eBPF enfrenta obstáculos estruturais severos:

### 2.1 O "Compiler Gap" em C e Rust (via LLVM)
O verificador do kernel Linux (`kernel/bpf/verifier.c`) opera como um interpretador abstrato sobre o bytecode carregado, rastreando o tipo exato (`SCALAR_VALUE`, `PTR_TO_CTX`, `PTR_TO_PACKET`, `PTR_TO_MAP_VALUE_OR_NULL`, `PTR_TO_STACK`) e os intervalos numéricos (`umin_value`, `umax_value`, `smin_value`, `smax_value`) de cada registrador $R0$–$R10$.

Quando C ou Rust (via *Aya* / `libbpf`) compilam código através do backend genérico do LLVM, ocorrem três falhas recorrentes:
1. **Perda de Proveniência em Aritmética de Ponteiros**: O LLVM frequentemente reordena `if (ptr + sizeof(hdr) <= data_end)` ou combina somas de offsets em registradores temporários de modo que o verificador não consegue associar a prova de limite ao ponteiro original (`PTR_TO_PACKET`), rejeitando programas válidos com `invalid access to packet`.
2. **Clobbering e Invalidação Silenciosa após Helpers**: No eBPF, toda instrução `BPF_CALL` destrói os registradores voláteis $R1$–$R5$, e certos helpers (como `bpf_skb_pull_data` ou `bpf_xdp_adjust_head`) invalidam todos os ponteiros de pacote previamente validados. Em C, reutilizar um ponteiro antigo só falha no momento do carregamento no kernel.
3. **Estouro de Pilha de 512 Bytes e Explosão de Caminhos**: Estruturas intermediárias criadas por monomorfização ou *spills* de registradores facilmente estouram os 512 bytes de stack por frame, ou geram grafos CFG que ultrapassam o limite de instruções analisadas pelo verificador.

### 2.2 Onde a Arquitetura do Arandu Ajuda (e Seus Limites Honestos)
* **O que o OSSA e a AMIR resolvem em compilação**:
  * O modelo de empréstimos (`F2`) pode modelar `PacketView` como um empréstimo atrelado ao estado atual de `ctx`. Se o usuário invocar `ctx.adjustHead(delta)`, que exige `mut ref Context`, o empréstimo anterior de `PacketView` é encerrado estaticamente pelo borrow checker (`O002`/`O006`)!
  * O sistema de efeitos (`@Effects(NoAlloc, NoThrow, NoSuspend)`) barra qualquer dependência da runtime de heap ou de corrotinas.
  * O `LayoutEngine` calcula tamanhos exatos de frames locais antes da emissão ELF.
* **O que o compilador NÃO substitui**:
  * O verificador do kernel Linux continua sendo a autoridade final em tempo de carga (`bpf(BPF_PROG_LOAD)`). Portanto, a conformidade de um backend eBPF exige testes reais de carregamento no verificador do kernel Linux em CI, e não apenas garantias teóricas do type checker.

---

## 3. Explicação em Nível de Guia (Guide-Level Explanation)

### 3.1 Escrevendo um Programa XDP com Invalidação Segura de Pacote

```arandu
import std.ebpf.xdp as xdp
import std.ebpf.maps as maps
import std.net.eth as eth
import std.net.ipv4 as ipv4

// Definição declarativa de mapa BPF (emite seção .maps com BTF)
const DROP_COUNTER = maps.PerCpuArray[u32, u64].withMaxEntries(256)

@Xdp
@Effects(NoAlloc, NoThrow, NoSuspend)
public func filterIpv4(ctx: mut ref xdp.Context): xdp.Action {
    // `packet()` empresta `ctx` de forma compartilhada (`ref xdp.Context`)
    let pkt = ctx.packet()

    // `readHeader` gera a guarda canônica `ptr + sizeof(T) <= data_end`
    let ethHdr = pkt.readHeader[eth.EthernetHeader](0) or {
        return xdp.Action.Pass
    }

    if ethHdr.etherType != eth.ETHER_TYPE_IPV4 {
        return xdp.Action.Pass
    }

    let ipHdr = pkt.readHeader[ipv4.Ipv4Header](eth.ETH_HDR_LEN) or {
        return xdp.Action.Pass
    }

    // Acesso a mapa BPF tipado: lookup retorna Option[mut ref u64]
    if ipHdr.protocol == ipv4.IPPROTO_UDP {
        let key: u32 = 1
        if DROP_COUNTER.lookupMut(ref key) is Option.Some(counter) {
            *counter = *counter + 1
        }
        return xdp.Action.Drop
    }

    return xdp.Action.Pass
}
```

### 3.2 Prevenção Estática de Uso de Ponteiro de Pacote Invalidado
Se o desenvolvedor chamar uma operação que altera a geometria do pacote (`ctx.adjustHead`), o empréstimo `pkt` não pode continuar vivo após a mutação:
```arandu
@Xdp
public func stripVlan(ctx: mut ref xdp.Context): xdp.Action {
    let pkt = ctx.packet()
    let ethHdr = pkt.readHeader[eth.EthernetHeader](0) or { return xdp.Action.Pass }

    // Mutação exclusiva de `ctx`: encerra a janela de empréstimo de `pkt` (F2.2)
    if ctx.adjustHead(4) != 0 {
        return xdp.Action.Aborted
    }

    // Se o código tentasse usar `ethHdr` aqui, o borrow checker emitiria O006!
    // É obrigatório adquirir uma nova view validada contra os novos data/data_end:
    let newPkt = ctx.packet()
    let _newEth = newPkt.readHeader[eth.EthernetHeader](0) or { return xdp.Action.Aborted }
    return xdp.Action.Pass
}
```

---

## 4. Explicação em Nível de Referência (Reference-Level Explanation)

### 4.1 Convenção de Registradores e Limites da ISA eBPF
A máquina virtual eBPF dispõe de 11 registradores de 64 bits e uma pilha fixa de 512 bytes:
* `$R0`: Valor de retorno de chamadas de helper e código de saída do programa;
* `$R1`–`$R5`: Argumentos de entrada para helpers do kernel (voláteis / *caller-saved*);
* `$R6`–`$R9`: Registradores preservados entre chamadas de helpers (*callee-saved*);
* `$R10`: Ponteiro de frame de pilha somente-leitura (`fp`).

Regras de emissão do `arandu_backend_ebpf`:
1. O ponteiro `ctx` recebido em `$R1` no prólogo da função de entrada é imediatamente copiado para um registrador callee-saved (`$R6`), garantindo que chamadas a helpers (`BPF_CALL`) não destruam o ponteiro de contexto.
2. O tamanho total dos slots de pilha alocados por função (incluindo *spill slots* do alocador de registradores e chamadas BPF-to-BPF) é verificado contra o limite de 512 bytes.

### 4.2 Emissão Cooperativa de Guardas de Limite (*Bounds Guard Lowering*)
O verificador do kernel Linux exige um padrão sintático específico de instruções para reconhecer limites de pacote:
1. Carregar `data = *(u32 *)(ctx + offset_data)` em `$Ra` (`PTR_TO_PACKET`);
2. Copiar `$Rb = $Ra` e somar o deslocamento constante imediato: `$Rb += offset + sizeof(T)`;
3. Carregar `data_end = *(u32 *)(ctx + offset_data_end)` em `$Rc` (`PTR_TO_PACKET_END`);
4. Emitir salto condicional direto `if $Rb > $Rc goto bb_err` **sem** derramar (`spill`) `$Ra` ou `$Rb` para a pilha entre a soma e a comparação;
5. No ramo `bb_ok`, realizar as leituras a partir de `$Ra + offset`.

Na AMIR, `pkt.readHeader[T](offset)` é rebaixado para um bloco intrínseco atômico que impede que passes como GVN ou LICM separem a adição do ponteiro do terminador de comparação.

### 4.3 Laços Limitados: `comptime for` vs. `BPF_LOOP`
Para evitar que o verificador rejeite laços como potencialmente infinitos:
* **Laços curtos constantes (`N <= 16`)**: Expandidos estaticamente via `comptime for` na AMIR.
* **Laços maiores com limite conhecido**: Emitidos com variável de indução em registrador callee-saved (`$R7`), incremento estritamente monotônico e comparação explícita `JLT` no cabeçalho do loop (suportado pelo verificador desde Linux 5.3) ou via iteradores numéricos do kernel (`bpf_iter_num_new` / `bpf_iter_num_next` / `bpf_iter_num_destroy` em alvos Linux $\ge 6.4$).

### 4.4 Seções ELF, Mapas e Relocações CO-RE (`.BTF` e `.BTF.ext`)
O backend gera um objeto ELF (`EM_BPF`, `64-bit`, LSB/MSB conforme `TargetInfo`) contendo:
* Seções de programa nomeadas pelo atributo (`xdp`, `tc`, `socket`, `tracepoint/<categoria>/<evento>`);
* Seção `.maps` no formato BTF-defined maps (`btf_key_type_id`, `btf_value_type_id`, `max_entries`);
* Seção `.BTF` descrevendo os tipos concretos do módulo;
* Seção `.BTF.ext` registrando relocações `BPF_CORE_FIELD_BYTE_OFFSET` para acessos a estruturas do kernel, permitindo *Compile Once – Run Everywhere* (CO-RE) via `libbpf`/carregadores compatíveis.

---

## 5. Invariantes de Arquitetura e Desvantagens (Drawbacks & Invariants)

### 5.1 Invariantes Preservados
* **Pureza das Queries Salsa**: O compilador nunca lê `/sys/kernel/btf/vmlinux` implicitamente dentro de queries tracked. Definições de tipos do kernel entram como módulos `.aru` normais gerados previamente ou como inputs registrados na DB.
* **Isolamento de Crates**: `arandu_backend_ebpf` consome apenas `arandu_middle` / `arandu_mir`, sem introduzir dependências de kernel no frontend.

### 5.2 Limitações e Desvantagens
* **Subconjunto Estrito da Linguagem**: Recursos que dependem de heap (`Vec`, `String`, `GenRef`), recursão, polimorfismo dinâmico indireto não resolvido e ponto flutuante em hardware (proibido no kernel Linux) são rejeitados pelo validador de alvo eBPF.
* **Dependência de Testes em Kernel Linux**: Embora a geração do ELF seja multiplataforma, a prova final de aceitação do bytecode exige execução do verificador do Linux (via runner Linux com permissão `CAP_BPF` ou `bpftool prog load`).

---

## 6. Racional e Alternativas (Rationale & Alternatives)

* **Alternativa A: Reutilizar o futuro backend LLVM (`RFC 0031`) com `--target=bpfel`:**
  * *Por que não é a estratégia primária?* O otimizador genérico do LLVM aplica transformações escalares que frequentemente invalidam as heurísticas do verificador do Linux. Como a ISA eBPF possui apenas ~100 opcodes e mapeia diretamente sobre a AMIR, um emissor dedicado oferece controle cirúrgico sobre a preservação de registradores e guardas de pacote.
* **Alternativa B: Emitir C (`arandu_backend_c`) e invocar `clang -target bpf`:**
  * *Rejeitada* como caminho final por exigir `clang` e cabeçalhos C externos na máquina do usuário, embora possa servir como oráculo comparativo durante o desenvolvimento.

---

## 7. Arte Prévia (Prior Art)

* **Linux Kernel Verifier (`kernel/bpf/verifier.c`)**: Documentação oficial do kernel Linux sobre rastreamento de estado de registradores, *pruning* e limites de pilha.
* **Aya (Rust)**: Demonstrou a viabilidade de escrever programas eBPF e carregadores BTF sem depender de `libbpf` em C, mas ainda sofre com rejeições induzidas pelo backend LLVM do `rustc`.
* **Pesquisa Acadêmica em Compilação Cooperativa (*Clef*, *ePass*, *K2*, *PREVAIL*)**: Estudos formais sobre interpretação abstrata de eBPF (PLDI/OSDI/ASPLOS) demonstrando como preservar invariantes de limite durante o lowering.

---

## 8. Questões em Aberto (Unresolved Questions)

1. **Carregador User-Space**: O Arandu fornecerá uma biblioteca `std.ebpf.loader` para o host Linux (via syscall `bpf(2)`) ou delegará o carregamento a ferramentas padrão (`bpftool`, `libbpf`, `aya`) no primeiro recorte?
2. **Versão Mínima de Kernel Alvo**: Qual será o baseline padrão de instruções (`v1` Linux 4.18, `v3` Linux 5.10 com `ALU32`, ou `v4` Linux 6.6+)?

---

## 9. Possibilidades Futuras (Future Possibilities)

* Simulador leve de interpretação abstrata em `arandu_backend_ebpf` capaz de diagnosticar em compilação (em Windows, macOS ou Linux sem root) padrões de registradores que o verificador do kernel rejeitaria.
* Geração integrada de esqueletos tipados Host + eBPF num único projeto `arandu.toml`.
