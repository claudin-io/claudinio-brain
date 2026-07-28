<h1 align="center">Claudinio Brain</h1>

<p align="center">
  <strong>Memória bitemporal em grafo de conhecimento para agentes de IA.</strong><br>
  Um binário, um arquivo, sem servidor e sem modelo no caminho de escrita.
</p>

<p align="center">
  <a href="LICENSE"><img alt="License: MIT" src="https://img.shields.io/badge/license-MIT-blue.svg"></a>
  <a href="https://github.com/claudin-io/claudinio-brain/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/claudin-io/claudinio-brain/actions/workflows/ci.yml/badge.svg"></a>
  <img alt="Rust" src="https://img.shields.io/badge/rust-1.95%2B-orange.svg">
  <img alt="Platforms" src="https://img.shields.io/badge/platform-macOS%20%7C%20Linux-lightgrey">
</p>

<p align="center">
  <a href="README.md">English</a> ·
  <a href="README.pt-BR.md">Português</a>
</p>

---

Dê um banco vetorial a um agente e pergunte como um serviço autentica. Ele vai
encontrar todas as respostas que alguém já escreveu e escolher uma. Não tem como
saber qual ainda é verdade, porque "usamos JWT" e "usávamos JWT" são a mesma
frase para uma medida de similaridade.

O `brain` guarda fatos numa linha do tempo, não numa pilha. Escrever um valor
novo não sobrescreve o antigo — **fecha** ele. Um registro só responde as duas
perguntas:

```console
$ brain remember --subject auth --predicate strategy --value "JWT" \
    --at 2026-01-01 --source adr-004
created: auth strategy JWT

$ brain remember --subject auth --predicate strategy --value "server-side sessions" \
    --at 2026-06-01 --source adr-011
superseded: auth strategy server-side sessions

$ brain get auth strategy
auth strategy server-side sessions

$ brain get auth strategy --as-of 2026-03-01
auth strategy JWT

$ brain history auth strategy
[2026-01-01T00:00:00Z] auth strategy JWT  (until 2026-06-01T00:00:00Z)
[2026-06-01T00:00:00Z] auth strategy server-side sessions  (current)
```

Nada foi apagado, e nada precisou ser reembeddado para isso funcionar.

## Um fato é qualquer coisa que valha mais que uma sessão

Sujeito, predicado, valor. Não há nada específico de domínio no modelo — não
existe schema para declarar, e um predicado é só uma palavra:

```console
$ brain remember --subject api_gateway --predicate timeout --value 30 --unit s
$ brain remember --subject checkout_service --predicate owner --value "platform-team"
$ brain remember --subject release_1_4 --predicate freeze_date --value 2026-08-15
$ brain remember --subject André --predicate team --value "platform"
```

Uma decisão e o motivo dela, um valor de config, um responsável, um prazo, uma
versão de schema, uma restrição que alguém falou em voz alta, onde no código a
resposta de verdade mora. Qualquer coisa que um agente teria que redescobrir,
chutar, ou perderia quando a sessão acabasse.

## Por que bitemporal

Duas linhas do tempo, rastreadas separadamente:

- **tempo de validade** — quando o fato foi verdade no mundo.
- **tempo de transação** — quando o brain ficou sabendo.

Manter as duas é o que permite distinguir três coisas que um único timestamp
colapsa em uma — e elas significam coisas muito diferentes para um agente:

| resultado | significado |
|---|---|
| **superseded** | mudou. O valor antigo *era* verdade, e deixou de ser. |
| **corrected** | estávamos errados. O valor antigo nunca foi verdade, então é retratado. |
| **reasserted** | falaram a mesma coisa de novo. Reforça, não duplica. |

Uma retratação deliberadamente **não** é o inverso de uma supersessão: ela não
reabre o que o fato retratado tinha fechado. "Isso estava errado" deixa aquele
período genuinamente desconhecido, e inventar uma resposta seria pior do que
admitir a lacuna.

## Relações são fatos

`link A rel B` é um fato cujo objeto é uma entidade, então o grafo herda
bitemporalidade de graça — uma dependência que mudou é um intervalo fechado, não
uma linha deletada. Isso também significa que a resposta pode morar num lugar
que as palavras da pergunta nunca alcançam:

```console
$ brain link checkout_service depends_on payments_db
$ brain remember --subject payments_db --predicate region --value "eu-west-1"

$ brain recall "which region does checkout_service data live in" --limit 3
checkout_service owner platform-team
payments_db region eu-west-1
checkout_service depends_on payments_db
```

"eu-west-1" não compartilha nenhuma palavra com a pergunta, e `payments_db`
também não — a pergunta nunca o nomeia. Está um salto além da entidade que *foi*
nomeada, e a relação é o mapa até lá.

## Como o recall funciona

Quatro canais independentes buscam candidatos, e *reciprocal rank fusion*
combina os resultados. Fundir em vez de escolher um é o ponto: concordância
entre sinais independentes é, por si só, evidência.

| canal | encontra |
|---|---|
| **bm25** | palavras, via FTS5. Insensível a acento, então "andre" acha "André". |
| **alias** | entidades que a pergunta nomeia diretamente, pela chave ou por outro nome. |
| **semantic** | paráfrases, via embeddings estáticos compilados no binário. |
| **graph** | fatos alcançados caminhando pelas relações a partir do que foi nomeado. |

Tudo é filtrado temporalmente *antes* do ranqueamento, então o recall responde
com o que é verdade, não com tudo que já foi registrado. Um fato retratado não
aparece nem em `--as-of` nem em `--history`: ele nunca foi verdade, então
repeti-lo seria mentir.

O canal semântico é uma tabela de **embeddings estáticos** — uma consulta
token-para-vetor, não um transformer. Sem runtime ONNX, sem download, sem
toolchain C++ e sem amostragem, que é o que torna o recall reproduzível o
bastante para as baselines de eval existirem.

## Nomes

Uma entidade é guardada sob uma chave, mas as pessoas perguntam com outras
palavras.

```console
$ brain alias payments_db "the payments database"     # um nome que você declara
"the_payments_database" now names payments_db

$ brain recall "which team is andre on" --learn --limit 3
André team platform
checkout_service depends_on payments_db
checkout_service owner platform-team
(learned: "andre" names André)

$ brain entity "André"
André (andré)
  also: andre (learned)
  André team platform
```

A pergunta perdeu o acento, então não nomeou nada — identidade é exata, e
`andre` não é `andré`. O BM25 respondeu mesmo assim, porque a busca é a camada
tolerante, e o nome que funcionou foi guardado.

Os dois têm níveis de confiança bem diferentes, e essa separação sustenta o
resto:

- Um alias **declarado** decide identidade. Fatos futuros sobre "the payments
  database" caem em `payments_db`.
- Um alias **aprendido** só amplia a busca. Um palpite que pudesse decidir
  identidade deixaria uma única pergunta bem formulada enxertar todo o histórico
  futuro de uma entidade no nó errado, sem nada em nenhuma saída denunciando isso.

O aprendizado fica desligado a menos que você peça (`--learn`), porque uma
leitura que escreve é uma leitura que não pode ser repetida. `brain entity <nome>`
mostra todos os nomes de uma coisa e de que tipo cada um é.

## Isolamento

Um brain é exatamente um arquivo SQLite, e duas propriedades são impostas em vez
de prometidas:

- **`ATTACH` é impossível.** `SQLITE_LIMIT_ATTACHED` é zero em toda conexão,
  então nenhuma query alcança um segundo arquivo. Sem isso, uma instrução
  forjada poderia juntar os fatos de outro cliente no mesmo resultado.
- **Um arquivo só é um brain se ele disser que é.** `open` exige o marcador
  `brain_id`, então um `.db` qualquer nunca é adotado. Arquivos nascem `0600`.

Toda resposta JSON carrega `brain_id`, `brain_label` e `brain_path` — o último
porque copiar um arquivo de brain duplica o id, então identidade sozinha não
distingue duas cópias.

`brain where` explica qual brain seria usado, e por quê. A escada de resolução
tem oito degraus e nenhum fallback silencioso: um diretório sem brain é um erro,
nunca o global.

## Instalação

Um binário pronto. Sem toolchain Rust, sem compilador C, nada para compilar:

```bash
curl -fsSL https://raw.githubusercontent.com/claudin-io/claudinio-brain/main/install.sh | sh
```

```powershell
irm https://raw.githubusercontent.com/claudin-io/claudinio-brain/main/install.ps1 | iex
```

Ele vai para `~/.local/bin` (`%LOCALAPPDATA%\Programs\brain` no Windows), e o
download é conferido contra o `SHA256SUMS` do release antes de ser instalado.
`BRAIN_INSTALL_DIR` escolhe outro lugar; `BRAIN_VERSION` escolhe um release —
`nightly` acompanha a `main`, recompilado a cada push.

| plataforma | build |
|---|---|
| macOS | `aarch64-apple-darwin`, `x86_64-apple-darwin` |
| Linux | `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl` |
| Windows | `x86_64-pc-windows-msvc` |

Os builds de Linux são musl estáticos: não têm piso de glibc, então rodam tanto
em distro antiga quanto em Alpine.

### A partir do código

Precisa de um compilador C (para `onig`, SQLite embutido e `sqlite-vec`). Um
compilador C++ deliberadamente **não** é necessário — veja
[docs/stack-notes.md](docs/stack-notes.md).

```bash
cargo install --git https://github.com/claudin-io/claudinio-brain
```

Ou a partir de um clone:

```bash
git clone https://github.com/claudin-io/claudinio-brain.git
cd claudinio-brain
cargo build --release        # target/release/brain
```

O binário de release é um arquivo único e autocontido de cerca de 14 MB —
SQLite, o índice vetorial e 7,3 MB de pesos quantizados estão todos compilados
dentro. Nada é baixado em tempo de execução, e o `brain` nunca faz requisição de
rede: `model2vec-rs` é compilado com `local-only`, o que remove todo caminho de
rede em tempo de compilação.

## Comandos

```
init       Cria um brain novo
where      Mostra qual brain seria usado aqui, e por quê
stats      Reporta a identidade e o conteúdo do brain
remember   Registra um fato
link       Registra uma relação entre duas entidades
get        Lê o valor atual, ou o valor num instante passado
recall     Busca no brain com uma pergunta em linguagem natural
history    Mostra a trajetória completa de um par sujeito/predicado
entity     Mostra o que se sabe de uma entidade, e a que ela se conecta
why        Mostra de onde um fato veio e o que aconteceu com ele
retract    Marca um fato como nunca tendo sido verdade
alias      Dá outro nome a uma entidade, lista os nomes, ou remove um
reindex    Reconstrói o índice vetorial a partir dos embeddings guardados
predicate  Corrige a cardinalidade de um predicado
```

Todo comando aceita `--json`, e toda resposta JSON é carimbada com o brain que a
produziu. `--brain <caminho>`, `--use <nome>` e `--global` escolhem com qual
brain falar.

## MCP

`brain serve` fala MCP sobre stdio, então um agente pode usar um brain como
superfície de ferramentas. Nove: `remember`, `link`, `recall`, `get`, `history`,
`entity`, `why`, `retract`, `alias`.

```json
{
  "mcpServers": {
    "brain": { "command": "brain", "args": ["serve", "--global"] }
  }
}
```

O servidor resolve o brain uma vez, na inicialização, pela mesma escada de oito
degraus, e fica preso a ele pela sessão inteira — por isso a identidade é dita
uma vez nas instruções do servidor em vez de carimbada em toda resposta, ao
contrário do CLI, onde cada invocação poderia apontar para um arquivo diferente.

As descrições das ferramentas carregam o que é fácil errar: que um valor que
*mudou* pede `remember` e um que *nunca foi verdade* pede `retract`; que o
`entity` diz sob qual grafia a coisa já está guardada, porque identidade é exata
e dois históricos paralelos não têm conserto.

O `recall` não aprende nomes sem que você peça. Uma leitura que escreve é uma
leitura que não pode ser repetida.

## Usando a partir de um agente

`skills/claudinio-brain/` é uma [Agent Skill](https://agentskills.io) que ensina
um agente quando gravar um fato e quando apenas responder — incluindo as partes
fáceis de errar, como a diferença entre um valor que *mudou* e um que *nunca foi
verdade*.

```bash
npx skills add claudin-io/claudinio-brain     # ou copie o diretório para a pasta
                                              # de skills do seu agente
```

A skill assume o `brain` no `PATH` e não faz nenhuma requisição de rede.

## Evals

Testes provam correção; evals medem qualidade. Um brain pode satisfazer todo
invariante bitemporal e ainda ser inútil se o recall nunca trouxer a resposta.

```bash
cargo run --example eval                      # mede e falha em regressão
cargo run --example eval -- --misses          # ...e nomeia os casos ainda errados
```

Quatro suítes — retrieval, temporal, graph, alias — cada uma pontuada contra
cada canal isolado e fundido, para que a contribuição marginal de um canal seja
um número e não uma opinião. `evals/baseline.json` é versionado e o CI falha em
regressão, o que significa que melhorar um número exige atualizar a baseline no
mesmo commit, onde ela entra no diff e é revisada.

[evals/README.md](evals/README.md) explica como ler a tabela de ablação, e tem
uma seção sobre o que essas suítes **não** conseguem medir.

## Status

Pré-1.0. O formato em disco ainda não é estável e não há caminho de migração
entre versões de schema.

Construído e funcionando: o store selado e a escada de resolução, o modelo
bitemporal de fatos, os quatro canais de recall, nomes declarados e aprendidos, e
o servidor MCP. Três superfícies — o CLI, o servidor MCP e a biblioteca Rust —
todas sobre um núcleo só, então o que um agente vê é exatamente o que o
`brain recall` te mostra.

## Contribuindo

[CONTRIBUTING.md](CONTRIBUTING.md) cobre o setup e o que é uma mudança
mergeável. Relatos de segurança seguem o [SECURITY.md](SECURITY.md) — por favor
não abra uma issue pública para isso.

## Licença

MIT. Veja [LICENSE](LICENSE).
