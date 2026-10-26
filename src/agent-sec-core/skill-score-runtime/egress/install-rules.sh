#!/bin/sh
set -eu

NAT_CHAIN=ASC_GOST_NAT
FILTER_CHAIN=ASC_GOST_EGRESS

install_family() {
  tool="$1"
  restore="$2"
  for pair in "nat:$NAT_CHAIN" "filter:$FILTER_CHAIN"; do
    table="${pair%%:*}"
    chain="${pair#*:}"
    if "$tool" -w 5 -t "$table" -S "$chain" >/dev/null 2>&1; then
      echo "Refusing to replace existing $tool $table $chain" >&2
      return 1
    fi
  done
  local_match='-m addrtype --dst-type LOCAL'
  icmp_rule="-A $FILTER_CHAIN -p icmp -j ACCEPT"
  if [ "$tool" = ip6tables-nft ]; then
    # xt_addrtype can also mark these routes LOCAL; native nft fib does not.
    local_match="$local_match -m addrtype ! --dst-type ANYCAST,UNREACHABLE"
    icmp_rule=''
  fi
  "$restore" --noflush --wait 5 <<EOF
*nat
:$NAT_CHAIN - [0:0]
-A $NAT_CHAIN -m mark --mark 114514 -j RETURN
-A $NAT_CHAIN $local_match -j RETURN
-A $NAT_CHAIN -p tcp -j REDIRECT --to-ports 12345
-I OUTPUT 1 -j $NAT_CHAIN
COMMIT
*filter
:$FILTER_CHAIN - [0:0]
-A $FILTER_CHAIN -m mark --mark 114514 -j ACCEPT
-A $FILTER_CHAIN $local_match -j ACCEPT
$icmp_rule
-A $FILTER_CHAIN ! -p tcp -j REJECT
-I OUTPUT 1 -j $FILTER_CHAIN
COMMIT
EOF
}

# A failed installation leaves the namespace restricted and never signals ready.
install_family iptables-nft iptables-nft-restore
install_family ip6tables-nft ip6tables-nft-restore
# Keep the original immediate next-header semantics, including IPv6 extensions.
nft insert rule ip6 filter "$FILTER_CHAIN" ip6 nexthdr icmpv6 accept
for tool in iptables-nft ip6tables-nft; do
  "$tool" -w 5 -t nat -C OUTPUT -j "$NAT_CHAIN"
  "$tool" -w 5 -t filter -C OUTPUT -j "$FILTER_CHAIN"
done
