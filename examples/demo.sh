#!/usr/bin/env sh
# Builds a small brain with something actually going on in it.
#
# Enough entities to make a graph worth orbiting, and -- more to the point -- one
# of each thing that is hard to see in a list and obvious in a picture:
#
#   * a price superseded twice, and a fourth value dated in the future, which is
#     the newest thing known without being true yet;
#   * a discount that carries its own end, so it stops being the answer on the
#     date it names with nobody having to come back for it;
#   * a supplier that changed, so one edge is a closed interval rather than a
#     deleted row;
#   * a fact that was never true, retracted rather than overwritten, leaving the
#     period it covered genuinely unknown;
#   * a declared alias next to a learned one, which are trusted very differently;
#   * a multi-valued predicate, where values coexist instead of superseding.
#
#   sh examples/demo.sh                      # -> ./demo/brain.db
#   sh examples/demo.sh /tmp/cafe.db         # somewhere else
#   BRAIN_BIN=target/debug/brain sh examples/demo.sh
#
# Then:
#   brain --brain demo/brain.db studio
#   brain --brain demo/brain.db export --out demo/studio.html

set -eu

BRAIN_BIN="${BRAIN_BIN:-brain}"
TARGET="${1:-./demo/brain.db}"

command -v "$BRAIN_BIN" >/dev/null 2>&1 || [ -x "$BRAIN_BIN" ] || {
  echo "demo: cannot find \`$BRAIN_BIN\` -- set BRAIN_BIN, or cargo build first" >&2
  exit 1
}

rm -f "$TARGET"
mkdir -p "$(dirname "$TARGET")"
"$BRAIN_BIN" init "$TARGET" --label "Claudinio Café" >/dev/null

b() { "$BRAIN_BIN" --brain "$TARGET" "$@" >/dev/null; }
say() { printf '  %s\n' "$1"; }

# Cardinality is the supersession rule, so the two that are not one-at-a-time are
# declared up front rather than left to be inferred from the first write.
b predicate certificacao --cardinality multi
b predicate depende_de --cardinality multi

say "produtos e preços"
b remember --subject "Bourbon Amarelo" --predicate preco --value 32 --unit BRL --at 2025-01-15
b remember --subject "Bourbon Amarelo" --predicate preco --value 38 --unit BRL --at 2025-09-01
b remember --subject "Bourbon Amarelo" --predicate preco --value 41 --unit BRL --at 2026-03-01
# Dated ahead: the newest thing the brain knows, and not today's price. Neither
# `get` nor `--as-of` today returns it -- "now" means the instant you are asking
# about -- and the studio keeps it in view anyway, ringed in violet, because what
# is scheduled is a thing you open a viewer to find out.
b remember --subject "Bourbon Amarelo" --predicate preco --value 45 --unit BRL --at 2026-11-01
b remember --subject "Bourbon Amarelo" --predicate torra --value "média" --at 2025-01-15

b remember --subject "Catuaí Vermelho" --predicate preco --value 28 --unit BRL --at 2025-01-15
b remember --subject "Catuaí Vermelho" --predicate preco --value 30 --unit BRL --at 2026-02-10
b remember --subject "Catuaí Vermelho" --predicate torra --value "escura" --at 2025-01-15

b remember --subject "Geisha Especial" --predicate preco --value 120 --unit BRL --at 2025-06-01
b remember --subject "Geisha Especial" --predicate torra --value "clara" --at 2025-06-01

say "uma promoção que termina sozinha"
# A fact that carries its own end. Nothing has to be written for this to stop
# being the answer on the day it names, which is what makes something short-lived
# safe to record at all. Until then the studio draws it in yellow: still true,
# with its end already on it.
b remember --subject "Geisha Especial" --predicate desconto --value 15 --unit % \
  --at 2026-07-01 --until 2027-06-01

say "fornecedores"
b remember --subject "Fazenda Serra Azul" --predicate pais --value "Brasil" --at 2025-01-10
b remember --subject "Fazenda Serra Azul" --predicate cidade --value "Carmo de Minas" --at 2025-01-10
b remember --subject "Fazenda Serra Azul" --predicate altitude --value 1100 --unit m --at 2025-01-10
b remember --subject "Fazenda Serra Azul" --predicate certificacao --value "Rainforest Alliance" --at 2025-03-01
b remember --subject "Fazenda Serra Azul" --predicate certificacao --value "Orgânico IBD" --at 2025-11-20

b remember --subject "Cooperativa Sul de Minas" --predicate pais --value "Brasil" --at 2025-01-10
b remember --subject "Cooperativa Sul de Minas" --predicate cidade --value "Varginha" --at 2025-01-10

b remember --subject "Finca La Esmeralda" --predicate pais --value "Panamá" --at 2025-05-01
b remember --subject "Finca La Esmeralda" --predicate altitude --value 1650 --unit m --at 2025-05-01

say "relações (que também são fatos, e por isso também têm tempo)"
b link "Bourbon Amarelo" fornecido_por "Cooperativa Sul de Minas" --at 2025-01-15
# The supplier changed. The old edge is closed, not deleted: "who supplied this
# in June 2025" still has an answer.
b link "Bourbon Amarelo" fornecido_por "Fazenda Serra Azul" --at 2026-01-20
b link "Catuaí Vermelho" fornecido_por "Fazenda Serra Azul" --at 2025-01-15
b link "Geisha Especial" fornecido_por "Finca La Esmeralda" --at 2025-06-01

say "o erro: um fato que nunca foi verdade"
FACT=$("$BRAIN_BIN" --brain "$TARGET" --json remember \
  --subject "Geisha Especial" --predicate origem --value "Brasil" --at 2025-06-01 \
  | python3 -c 'import sys, json; print(json.load(sys.stdin)["fact"]["id"])')
# Not a supersession. Nobody ever bought a Brazilian Geisha from this roaster --
# the lot was mislabelled -- so the claim is retracted, and the period it covered
# is left genuinely unknown rather than backfilled with a guess.
b retract "$FACT" --reason "lote trocado com o Catuaí na planilha de entrada"
b remember --subject "Geisha Especial" --predicate origem --value "Panamá" --at 2025-06-01

say "pessoas e clientes"
b remember --subject "Marina" --predicate papel --value "engenheira de plataforma" --at 2025-02-01
b remember --subject "Diego" --predicate papel --value "comprador" --at 2025-02-01
b link "Diego" compra_de "Fazenda Serra Azul" --at 2025-02-01
b link "Diego" compra_de "Finca La Esmeralda" --at 2025-06-01

b remember --subject "Padaria do Zé" --predicate cidade --value "São Paulo" --at 2025-04-01
b remember --subject "Café Central" --predicate cidade --value "Curitiba" --at 2025-07-01
b link "Padaria do Zé" compra "Bourbon Amarelo" --at 2025-04-01
b link "Café Central" compra "Geisha Especial" --at 2025-07-01
b link "Café Central" compra "Catuaí Vermelho" --at 2026-05-01

say "infraestrutura (um brain guarda o que for)"
b remember --subject "api-pedidos" --predicate porta --value 8080 --at 2025-03-10
b remember --subject "api-pedidos" --predicate porta --value 8443 --at 2026-04-01
b remember --subject "banco-postgres" --predicate versao --value 16 --at 2025-03-10
b remember --subject "banco-postgres" --predicate versao --value 17 --at 2026-05-12
b remember --subject "servidor-sp1" --predicate regiao --value "sa-east-1" --at 2025-03-10
b link "api-pedidos" depende_de "banco-postgres" --at 2025-03-10
b link "api-pedidos" depende_de "fila-rabbit" --at 2025-08-14
b link "banco-postgres" hospedado_em "servidor-sp1" --at 2025-03-10
b link "fila-rabbit" hospedado_em "servidor-sp1" --at 2025-08-14
b link "Marina" responsavel_por "api-pedidos" --at 2025-03-10

say "nomes"
# Declared: decides identity, so later facts written under this name land here.
b alias "Fazenda Serra Azul" "Serra Azul"
b alias "Cooperativa Sul de Minas" "Coopersul"
# Learned: only widens retrieval. `--learn` is what makes a read write, which is
# why it is opt-in.
b recall "quanto custa o bourbon amarelo" --learn
b recall "de que pais vem o geisha especial" --learn

echo
echo "brain ready: $TARGET"
echo
echo "  $BRAIN_BIN --brain $TARGET studio"
echo "  $BRAIN_BIN --brain $TARGET export --out $(dirname "$TARGET")/studio.html"
