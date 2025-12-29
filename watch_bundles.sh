#!/bin/bash
# ==============================================================================
# Bundle Status Watcher for MEV Bot
# Monitors bundle submission results from Jito Block Engine
# ==============================================================================

set -e

# Configuration
JITO_ENDPOINT="${JITO_ENDPOINT:-https://mainnet.block-engine.jito.wtf}"
BUNDLE_API="${JITO_ENDPOINT}/api/v1/bundles"
POLL_INTERVAL="${POLL_INTERVAL:-2}"
MAX_BUNDLES="${MAX_BUNDLES:-100}"
WATCH_LOGS="${WATCH_LOGS:-true}"
LOG_FILE="${LOG_FILE:-/tmp/mev_bot.log}"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
PURPLE='\033[0;35m'
CYAN='\033[0;36m'
WHITE='\033[1;37m'
GRAY='\033[0;90m'
NC='\033[0m' # No Color

# Track seen bundles to avoid duplicates
declare -A SEEN_BUNDLES
declare -a BUNDLE_QUEUE

# Statistics
TOTAL_SUBMITTED=0
TOTAL_ACCEPTED=0
TOTAL_REJECTED=0
TOTAL_DROPPED=0
TOTAL_FINALIZED=0

print_header() {
    clear
    echo -e "${CYAN}"
    echo "╔══════════════════════════════════════════════════════════════════════════════╗"
    echo "║                      🚀 MEV Bundle Status Watcher                            ║"
    echo "╠══════════════════════════════════════════════════════════════════════════════╣"
    echo -e "║  Jito Endpoint: ${WHITE}${JITO_ENDPOINT}${CYAN}"
    printf "%-81s║\n" ""
    echo "║  Press Ctrl+C to stop                                                        ║"
    echo "╚══════════════════════════════════════════════════════════════════════════════╝"
    echo -e "${NC}"
}

print_stats() {
    local now=$(date '+%Y-%m-%d %H:%M:%S')
    echo -e "${GRAY}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
    echo -e "${WHITE}[$now]${NC} ${BLUE}STATS:${NC} " \
        "${CYAN}Submitted:${NC} $TOTAL_SUBMITTED | " \
        "${GREEN}Accepted:${NC} $TOTAL_ACCEPTED | " \
        "${GREEN}Finalized:${NC} $TOTAL_FINALIZED | " \
        "${RED}Rejected:${NC} $TOTAL_REJECTED | " \
        "${YELLOW}Dropped:${NC} $TOTAL_DROPPED"
    echo -e "${GRAY}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
}

# Format bundle ID for display (show first 16 chars)
format_bundle_id() {
    local id="$1"
    if [ ${#id} -gt 16 ]; then
        echo "${id:0:16}..."
    else
        echo "$id"
    fi
}

# Get bundle status from Jito API
get_bundle_status() {
    local bundle_id="$1"
    local response
    
    response=$(curl -s -X POST "$BUNDLE_API" \
        -H "Content-Type: application/json" \
        -d "{
            \"jsonrpc\": \"2.0\",
            \"id\": 1,
            \"method\": \"getBundleStatuses\",
            \"params\": [[\"$bundle_id\"]]
        }" 2>/dev/null)
    
    echo "$response"
}

# Parse and display bundle result
parse_bundle_result() {
    local result="$1"
    local bundle_id="$2"
    local now=$(date '+%H:%M:%S')
    local short_id=$(format_bundle_id "$bundle_id")
    
    # Check if null (bundle not found or not landed)
    if echo "$result" | grep -q '"value":\[null\]'; then
        echo -e "${GRAY}[$now]${NC} ${BLUE}⏳${NC} Bundle ${PURPLE}$short_id${NC} - Pending (not landed yet)"
        return 1
    fi
    
    # Parse confirmation status
    local status=$(echo "$result" | grep -oP '"confirmation_status"\s*:\s*"\K[^"]+' 2>/dev/null || echo "unknown")
    local slot=$(echo "$result" | grep -oP '"slot"\s*:\s*\K[0-9]+' 2>/dev/null | head -1 || echo "?")
    local err=$(echo "$result" | grep -oP '"err"\s*:\s*\K[^,}]+' 2>/dev/null || echo "null")
    
    case "$status" in
        "finalized")
            echo -e "${GRAY}[$now]${NC} ${GREEN}✅ FINALIZED${NC} Bundle ${PURPLE}$short_id${NC} at slot ${CYAN}$slot${NC}"
            ((TOTAL_FINALIZED++))
            SEEN_BUNDLES[$bundle_id]="finalized"
            return 0
            ;;
        "confirmed")
            echo -e "${GRAY}[$now]${NC} ${GREEN}🎯 CONFIRMED${NC} Bundle ${PURPLE}$short_id${NC} at slot ${CYAN}$slot${NC}"
            ((TOTAL_ACCEPTED++))
            SEEN_BUNDLES[$bundle_id]="confirmed"
            return 0
            ;;
        "processed")
            echo -e "${GRAY}[$now]${NC} ${YELLOW}⚙️  PROCESSED${NC} Bundle ${PURPLE}$short_id${NC} at slot ${CYAN}$slot${NC}"
            return 1
            ;;
        *)
            if [ "$err" != "null" ] && [ "$err" != "{\"Ok\":null}" ]; then
                echo -e "${GRAY}[$now]${NC} ${RED}❌ ERROR${NC} Bundle ${PURPLE}$short_id${NC}: ${RED}$err${NC}"
                ((TOTAL_REJECTED++))
                SEEN_BUNDLES[$bundle_id]="error"
                return 0
            else
                echo -e "${GRAY}[$now]${NC} ${YELLOW}❓ UNKNOWN${NC} Bundle ${PURPLE}$short_id${NC} status: $status"
                return 1
            fi
            ;;
    esac
}

# Watch bot logs for bundle submissions
watch_logs() {
    local log_source="$1"
    
    # Try different ways to get logs
    if [ -f "$LOG_FILE" ]; then
        tail -f "$LOG_FILE" 2>/dev/null
    elif pgrep -f "tre-arb-bot" > /dev/null; then
        # Follow the process stdout/stderr
        journalctl -f -u tre-arb-bot 2>/dev/null || \
        strace -p $(pgrep -f "tre-arb-bot" | head -1) -e write 2>&1
    else
        # Just monitor system journal
        journalctl -f 2>/dev/null
    fi
}

# Parse log line for bundle info
parse_log_line() {
    local line="$1"
    local now=$(date '+%H:%M:%S')
    
    # Match bundle submission log
    if echo "$line" | grep -q "Submitting bundle to Jito"; then
        local bundle_id=$(echo "$line" | grep -oP 'bundle_id\s*=\s*\K[a-zA-Z0-9]+' || echo "")
        local profit=$(echo "$line" | grep -oP 'profit\s*=\s*\K[0-9]+' || echo "?")
        local tip=$(echo "$line" | grep -oP 'tip\s*=\s*\K[0-9]+' || echo "?")
        
        if [ -n "$bundle_id" ]; then
            ((TOTAL_SUBMITTED++))
            local short_id=$(format_bundle_id "$bundle_id")
            # Add to tracking queue
            BUNDLE_QUEUE+=("$bundle_id")
            # Keep queue size manageable
            if [ ${#BUNDLE_QUEUE[@]} -gt $MAX_BUNDLES ]; then
                BUNDLE_QUEUE=("${BUNDLE_QUEUE[@]:1}")
            fi
            echo -e "${GRAY}[$now]${NC} ${CYAN}📤 SUBMITTED${NC} Bundle ${PURPLE}$short_id${NC} | Profit: ${GREEN}$profit${NC} lamports | Tip: ${YELLOW}$tip${NC} lamports"
        fi
    fi
    
    # Match bundle accepted
    if echo "$line" | grep -q "Bundle ACCEPTED"; then
        local bundle_id=$(echo "$line" | grep -oP 'bundle_id\s*=\s*\K[a-zA-Z0-9]+' || echo "")
        local slot=$(echo "$line" | grep -oP 'slot\s*=\s*\K[0-9]+' || echo "?")
        local validator=$(echo "$line" | grep -oP 'validator\s*=\s*\K[a-zA-Z0-9]+' || echo "?")
        
        if [ -n "$bundle_id" ]; then
            ((TOTAL_ACCEPTED++))
            local short_id=$(format_bundle_id "$bundle_id")
            local short_validator=$(format_bundle_id "$validator")
            SEEN_BUNDLES[$bundle_id]="accepted"
            echo -e "${GRAY}[$now]${NC} ${GREEN}🎯 ACCEPTED${NC} Bundle ${PURPLE}$short_id${NC} | Slot: ${CYAN}$slot${NC} | Validator: ${BLUE}$short_validator${NC}"
        fi
    fi
    
    # Match bundle finalized
    if echo "$line" | grep -q "Bundle FINALIZED"; then
        local bundle_id=$(echo "$line" | grep -oP 'bundle_id\s*=\s*\K[a-zA-Z0-9]+' || echo "")
        
        if [ -n "$bundle_id" ]; then
            ((TOTAL_FINALIZED++))
            local short_id=$(format_bundle_id "$bundle_id")
            SEEN_BUNDLES[$bundle_id]="finalized"
            echo -e "${GRAY}[$now]${NC} ${GREEN}✅ FINALIZED${NC} Bundle ${PURPLE}$short_id${NC}"
        fi
    fi
    
    # Match bundle rejected
    if echo "$line" | grep -q "Bundle REJECTED"; then
        local bundle_id=$(echo "$line" | grep -oP 'bundle_id\s*=\s*\K[a-zA-Z0-9]+' || echo "")
        local reason=$(echo "$line" | grep -oP 'reason\s*=\s*"\K[^"]+' || echo "$line" | grep -oP 'reason\s*=\s*\K[^,]+' || echo "unknown")
        
        if [ -n "$bundle_id" ]; then
            ((TOTAL_REJECTED++))
            local short_id=$(format_bundle_id "$bundle_id")
            SEEN_BUNDLES[$bundle_id]="rejected"
            echo -e "${GRAY}[$now]${NC} ${RED}❌ REJECTED${NC} Bundle ${PURPLE}$short_id${NC}"
            echo -e "    ${RED}Reason:${NC} $reason"
        fi
    fi
    
    # Match bundle dropped
    if echo "$line" | grep -q "Bundle DROPPED"; then
        local bundle_id=$(echo "$line" | grep -oP 'bundle_id\s*=\s*\K[a-zA-Z0-9]+' || echo "")
        local reason=$(echo "$line" | grep -oP 'reason\s*=\s*"\K[^"]+' || echo "$line" | grep -oP 'reason\s*=\s*\K[^,]+' || echo "unknown")
        
        if [ -n "$bundle_id" ]; then
            ((TOTAL_DROPPED++))
            local short_id=$(format_bundle_id "$bundle_id")
            SEEN_BUNDLES[$bundle_id]="dropped"
            echo -e "${GRAY}[$now]${NC} ${YELLOW}⚠️  DROPPED${NC} Bundle ${PURPLE}$short_id${NC}"
            echo -e "    ${YELLOW}Reason:${NC} $reason"
        fi
    fi
    
    # Match simulation failure details
    if echo "$line" | grep -q "SIMULATION_FAILURE"; then
        local details=$(echo "$line" | grep -oP 'SIMULATION_FAILURE.*' || echo "")
        if [ -n "$details" ]; then
            echo -e "    ${RED}Details:${NC} $details"
        fi
    fi
}

# Poll for bundle statuses
poll_bundle_statuses() {
    for bundle_id in "${BUNDLE_QUEUE[@]}"; do
        # Skip if already finalized or errored
        if [ "${SEEN_BUNDLES[$bundle_id]}" = "finalized" ] || \
           [ "${SEEN_BUNDLES[$bundle_id]}" = "error" ]; then
            continue
        fi
        
        local result=$(get_bundle_status "$bundle_id")
        if [ -n "$result" ] && [ "$result" != "" ]; then
            parse_bundle_result "$result" "$bundle_id"
        fi
    done
}

# Cleanup function
cleanup() {
    echo ""
    echo -e "${CYAN}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
    echo -e "${WHITE}Final Statistics:${NC}"
    echo -e "  ${CYAN}Total Submitted:${NC}  $TOTAL_SUBMITTED"
    echo -e "  ${GREEN}Total Accepted:${NC}   $TOTAL_ACCEPTED"
    echo -e "  ${GREEN}Total Finalized:${NC}  $TOTAL_FINALIZED"
    echo -e "  ${RED}Total Rejected:${NC}   $TOTAL_REJECTED"
    echo -e "  ${YELLOW}Total Dropped:${NC}    $TOTAL_DROPPED"
    
    if [ $TOTAL_SUBMITTED -gt 0 ]; then
        local success_rate=$(echo "scale=1; ($TOTAL_ACCEPTED + $TOTAL_FINALIZED) * 100 / $TOTAL_SUBMITTED" | bc 2>/dev/null || echo "?")
        echo -e "  ${WHITE}Success Rate:${NC}     ${success_rate}%"
    fi
    echo -e "${CYAN}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
    exit 0
}

trap cleanup SIGINT SIGTERM

# Show usage
show_usage() {
    echo "Usage: $0 [OPTIONS]"
    echo ""
    echo "Options:"
    echo "  -e, --endpoint URL    Jito endpoint (default: https://mainnet.block-engine.jito.wtf)"
    echo "  -i, --interval SECS   Polling interval in seconds (default: 2)"
    echo "  -l, --log FILE        Bot log file to watch (default: /tmp/mev_bot.log)"
    echo "  -b, --bundle ID       Query status of specific bundle ID"
    echo "  -w, --watch-only      Only watch logs, don't poll API"
    echo "  -p, --poll-only       Only poll API for specified bundles"
    echo "  -h, --help            Show this help message"
    echo ""
    echo "Examples:"
    echo "  $0                                    # Watch logs + poll for statuses"
    echo "  $0 -b abc123...                       # Check specific bundle status"
    echo "  $0 -e https://ny.mainnet.block-engine.jito.wtf"
    echo ""
    echo "Environment Variables:"
    echo "  JITO_ENDPOINT     Jito Block Engine endpoint"
    echo "  POLL_INTERVAL     Polling interval in seconds"
    echo "  LOG_FILE          Path to MEV bot log file"
}

# Parse arguments
SPECIFIC_BUNDLE=""
WATCH_ONLY=false
POLL_ONLY=false

while [[ $# -gt 0 ]]; do
    case $1 in
        -e|--endpoint)
            JITO_ENDPOINT="$2"
            BUNDLE_API="${JITO_ENDPOINT}/api/v1/bundles"
            shift 2
            ;;
        -i|--interval)
            POLL_INTERVAL="$2"
            shift 2
            ;;
        -l|--log)
            LOG_FILE="$2"
            shift 2
            ;;
        -b|--bundle)
            SPECIFIC_BUNDLE="$2"
            shift 2
            ;;
        -w|--watch-only)
            WATCH_ONLY=true
            shift
            ;;
        -p|--poll-only)
            POLL_ONLY=true
            shift
            ;;
        -h|--help)
            show_usage
            exit 0
            ;;
        *)
            echo "Unknown option: $1"
            show_usage
            exit 1
            ;;
    esac
done

# Main execution
print_header

# If specific bundle ID provided, just check that one
if [ -n "$SPECIFIC_BUNDLE" ]; then
    echo -e "${WHITE}Checking bundle status...${NC}"
    echo ""
    result=$(get_bundle_status "$SPECIFIC_BUNDLE")
    
    if [ -n "$result" ]; then
        echo -e "${GRAY}Raw response:${NC}"
        echo "$result" | jq . 2>/dev/null || echo "$result"
        echo ""
        parse_bundle_result "$result" "$SPECIFIC_BUNDLE"
    else
        echo -e "${RED}Failed to query bundle status${NC}"
    fi
    exit 0
fi

# Start watching
echo -e "${WHITE}Watching for bundle activity...${NC}"
echo ""

# Run in watch mode
if [ "$POLL_ONLY" = true ]; then
    echo -e "${YELLOW}Poll-only mode: Add bundle IDs to track with -b option${NC}"
    exit 0
fi

# Background polling for bundle statuses
if [ "$WATCH_ONLY" != true ]; then
    (
        while true; do
            sleep $POLL_INTERVAL
            poll_bundle_statuses
        done
    ) &
    POLL_PID=$!
    trap "kill $POLL_PID 2>/dev/null; cleanup" SIGINT SIGTERM
fi

# Print stats periodically in background
(
    while true; do
        sleep 30
        print_stats
    done
) &
STATS_PID=$!
trap "kill $STATS_PID 2>/dev/null; kill $POLL_PID 2>/dev/null; cleanup" SIGINT SIGTERM

# Main log watching loop - watch tre-arb-bot process output
# Try multiple sources for logs
if pgrep -f "tre-arb-bot" > /dev/null 2>&1; then
    # Bot is running - try to find its output
    # First check if running in current terminal
    BOT_PID=$(pgrep -f "tre-arb-bot" | head -1)
    echo -e "${GREEN}Found running bot (PID: $BOT_PID)${NC}"
    
    # Watch /proc/PID/fd/1 (stdout) and /proc/PID/fd/2 (stderr)
    if [ -r "/proc/$BOT_PID/fd/1" ]; then
        tail -f /proc/$BOT_PID/fd/1 /proc/$BOT_PID/fd/2 2>/dev/null | while read -r line; do
            parse_log_line "$line"
        done
    else
        # Fallback: grep from dmesg or syslog
        echo -e "${YELLOW}Cannot access process output directly. Watching system logs...${NC}"
        journalctl -f 2>/dev/null | grep --line-buffered -i "bundle" | while read -r line; do
            parse_log_line "$line"
        done
    fi
else
    # Bot not running - watch log file if exists
    if [ -f "$LOG_FILE" ]; then
        echo -e "${CYAN}Watching log file: $LOG_FILE${NC}"
        tail -f "$LOG_FILE" 2>/dev/null | while read -r line; do
            parse_log_line "$line"
        done
    else
        echo -e "${YELLOW}Bot not running and no log file found.${NC}"
        echo -e "${YELLOW}Start the bot or specify a log file with -l option.${NC}"
        echo ""
        echo -e "${WHITE}Waiting for bot to start...${NC}"
        
        # Wait for bot to start
        while ! pgrep -f "tre-arb-bot" > /dev/null 2>&1; do
            sleep 2
        done
        
        echo -e "${GREEN}Bot started! Beginning to watch...${NC}"
        exec "$0" "$@"
    fi
fi
