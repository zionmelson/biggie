#!/bin/bash
# ==============================================================================
# Run MEV Bot with Bundle Watcher
# Runs the MEV bot and captures bundle results in real-time
# ==============================================================================

set -e

cd /home/ubuntu/biggie

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
PURPLE='\033[0;35m'
CYAN='\033[0;36m'
WHITE='\033[1;37m'
GRAY='\033[0;90m'
NC='\033[0m'

# Log file for bundle tracking
BUNDLE_LOG="/tmp/mev_bundles_$(date +%Y%m%d_%H%M%S).log"
touch "$BUNDLE_LOG"

echo -e "${CYAN}"
echo "╔══════════════════════════════════════════════════════════════════════════════╗"
echo "║                   🚀 MEV Bot with Bundle Watcher                             ║"
echo "╠══════════════════════════════════════════════════════════════════════════════╣"
echo "║  Bundle log: $BUNDLE_LOG"
echo "║  Press Ctrl+C to stop                                                        ║"
echo "╚══════════════════════════════════════════════════════════════════════════════╝"
echo -e "${NC}"

# Stats
declare -i SUBMITTED=0 ACCEPTED=0 REJECTED=0 DROPPED=0 FINALIZED=0

# Process a log line and extract bundle info
process_line() {
    local line="$1"
    local now=$(date '+%H:%M:%S')
    
    # Submitting bundle
    if echo "$line" | grep -q "Submitting bundle to Jito"; then
        ((SUBMITTED++))
        local bundle_id=$(echo "$line" | grep -oP 'bundle_id=\K[a-zA-Z0-9]+' 2>/dev/null | head -c 20)
        local profit=$(echo "$line" | grep -oP 'profit=\K[0-9]+' 2>/dev/null)
        local tip=$(echo "$line" | grep -oP 'tip=\K[0-9]+' 2>/dev/null)
        local slots=$(echo "$line" | grep -oP 'slots_until_leader=\K[0-9]+' 2>/dev/null)
        
        echo -e "${GRAY}[$now]${NC} ${CYAN}📤 SUBMITTED${NC} ${PURPLE}${bundle_id}...${NC} | profit=${GREEN}${profit:-?}${NC} | tip=${YELLOW}${tip:-?}${NC} | slots=${BLUE}${slots:-?}${NC}"
        echo "[$now] SUBMITTED bundle_id=$bundle_id profit=$profit tip=$tip slots=$slots" >> "$BUNDLE_LOG"
    fi
    
    # Bundle accepted
    if echo "$line" | grep -q "Bundle ACCEPTED"; then
        ((ACCEPTED++))
        local bundle_id=$(echo "$line" | grep -oP 'bundle_id=\K[a-zA-Z0-9]+' 2>/dev/null | head -c 20)
        local slot=$(echo "$line" | grep -oP 'slot=\K[0-9]+' 2>/dev/null)
        local validator=$(echo "$line" | grep -oP 'validator=\K[a-zA-Z0-9]+' 2>/dev/null | head -c 12)
        
        echo -e "${GRAY}[$now]${NC} ${GREEN}🎯 ACCEPTED${NC}  ${PURPLE}${bundle_id}...${NC} | slot=${CYAN}${slot:-?}${NC} | validator=${BLUE}${validator:-?}...${NC}"
        echo "[$now] ACCEPTED bundle_id=$bundle_id slot=$slot validator=$validator" >> "$BUNDLE_LOG"
    fi
    
    # Bundle finalized
    if echo "$line" | grep -q "Bundle FINALIZED"; then
        ((FINALIZED++))
        local bundle_id=$(echo "$line" | grep -oP 'bundle_id=\K[a-zA-Z0-9]+' 2>/dev/null | head -c 20)
        
        echo -e "${GRAY}[$now]${NC} ${GREEN}✅ FINALIZED${NC} ${PURPLE}${bundle_id}...${NC}"
        echo "[$now] FINALIZED bundle_id=$bundle_id" >> "$BUNDLE_LOG"
    fi
    
    # Bundle rejected
    if echo "$line" | grep -q "Bundle REJECTED"; then
        ((REJECTED++))
        local bundle_id=$(echo "$line" | grep -oP 'bundle_id=\K[a-zA-Z0-9]+' 2>/dev/null | head -c 20)
        local reason=$(echo "$line" | grep -oP 'reason=\K[^,]+' 2>/dev/null | head -c 80)
        
        echo -e "${GRAY}[$now]${NC} ${RED}❌ REJECTED${NC}  ${PURPLE}${bundle_id}...${NC}"
        if [ -n "$reason" ]; then
            echo -e "         ${RED}→ Reason:${NC} $reason"
        fi
        echo "[$now] REJECTED bundle_id=$bundle_id reason=$reason" >> "$BUNDLE_LOG"
    fi
    
    # Bundle dropped
    if echo "$line" | grep -q "Bundle DROPPED"; then
        ((DROPPED++))
        local bundle_id=$(echo "$line" | grep -oP 'bundle_id=\K[a-zA-Z0-9]+' 2>/dev/null | head -c 20)
        local reason=$(echo "$line" | grep -oP 'reason=\K[^,]+' 2>/dev/null | head -c 80)
        
        echo -e "${GRAY}[$now]${NC} ${YELLOW}⚠️  DROPPED${NC}  ${PURPLE}${bundle_id}...${NC}"
        if [ -n "$reason" ]; then
            echo -e "         ${YELLOW}→ Reason:${NC} $reason"
        fi
        echo "[$now] DROPPED bundle_id=$bundle_id reason=$reason" >> "$BUNDLE_LOG"
    fi
    
    # Simulation failure details
    if echo "$line" | grep -q "SIMULATION_FAILURE"; then
        local sig=$(echo "$line" | grep -oP 'sig=\K[a-zA-Z0-9_]+' 2>/dev/null | head -c 16)
        local ix=$(echo "$line" | grep -oP 'ix=\K[0-9?]+' 2>/dev/null)
        local error=$(echo "$line" | grep -oP 'error=\K[a-zA-Z0-9x_]+' 2>/dev/null)
        
        echo -e "         ${RED}→ Sim Fail:${NC} sig=${sig:-?}... ix=${ix:-?} error=${error:-unknown}"
    fi
    
    # Opportunity found (for context)
    if echo "$line" | grep -q "ARB_OPP"; then
        local pair=$(echo "$line" | grep -oP 'pair=\K[A-Z/-]+' 2>/dev/null)
        local profit=$(echo "$line" | grep -oP 'profit_lamports=\K[0-9]+' 2>/dev/null)
        local dex1=$(echo "$line" | grep -oP 'buy_dex=\K[A-Za-z]+' 2>/dev/null)
        local dex2=$(echo "$line" | grep -oP 'sell_dex=\K[A-Za-z]+' 2>/dev/null)
        
        echo -e "${GRAY}[$now]${NC} ${BLUE}💡 OPPORTUNITY${NC} ${WHITE}$pair${NC} | profit=${GREEN}$profit${NC} | ${dex1} → ${dex2}"
    fi
}

# Print summary on exit
print_summary() {
    echo ""
    echo -e "${CYAN}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
    echo -e "${WHITE}Bundle Summary:${NC}"
    echo -e "  ${CYAN}Submitted:${NC}  $SUBMITTED"
    echo -e "  ${GREEN}Accepted:${NC}   $ACCEPTED"
    echo -e "  ${GREEN}Finalized:${NC}  $FINALIZED"
    echo -e "  ${RED}Rejected:${NC}   $REJECTED"
    echo -e "  ${YELLOW}Dropped:${NC}    $DROPPED"
    
    if [ $SUBMITTED -gt 0 ]; then
        local landed=$((ACCEPTED + FINALIZED))
        local rate=$(echo "scale=1; $landed * 100 / $SUBMITTED" | bc 2>/dev/null || echo "?")
        echo -e "  ${WHITE}Land Rate:${NC}  ${rate}%"
    fi
    
    echo -e ""
    echo -e "  ${GRAY}Bundle log saved to: $BUNDLE_LOG${NC}"
    echo -e "${CYAN}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
}

# Print periodic stats
print_stats() {
    while true; do
        sleep 60
        local now=$(date '+%H:%M:%S')
        echo -e ""
        echo -e "${GRAY}[$now]${NC} ${WHITE}STATS:${NC} submitted=${CYAN}$SUBMITTED${NC} accepted=${GREEN}$ACCEPTED${NC} finalized=${GREEN}$FINALIZED${NC} rejected=${RED}$REJECTED${NC} dropped=${YELLOW}$DROPPED${NC}"
        echo -e ""
    done
}

trap print_summary EXIT

# Start stats printer in background
print_stats &
STATS_PID=$!
trap "kill $STATS_PID 2>/dev/null; print_summary; exit 0" INT TERM

echo -e "${WHITE}Starting MEV bot...${NC}"
echo ""

rm -f /tmp/tre-arb.log

# Run bot and filter for bundle-related output
# The bot outputs to stderr (via tracing), so we redirect stderr to stdout
./run_lowlatency.sh 2>&1 | while IFS= read -r line; do
    # Check if line contains bundle-related info
    if echo "$line" | grep -qE "bundle|Bundle|BUNDLE|ARB_OPP|SIMULATION_FAILURE"; then
        process_line "$line"
    fi
    
    # Also show important status messages
    if echo "$line" | grep -qE "connected|Started|Pipeline|Geyser|Jito|ERROR|WARN.*fail|subscriber"; then
        echo -e "${GRAY}[$(date '+%H:%M:%S')]${NC} $line"
    fi
done
