#!/bin/bash
# ==============================================================================
# Run MEV Bot Overnight
# Runs the bot in a tmux session with logging, safe to disconnect
# ==============================================================================

set -e
cd /home/ubuntu/biggie

# Config
SESSION_NAME="biggie"
LOG_DIR="/home/ubuntu/biggie/logs"
LOG_FILE="$LOG_DIR/biggie_$(date +%Y%m%d_%H%M%S).log"
PID_FILE="/home/ubuntu/biggie/biggie.pid"

# Create log directory
mkdir -p "$LOG_DIR"

# Colors
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
NC='\033[0m'

case "${1:-start}" in
    start)
        # Check if already running
        if tmux has-session -t "$SESSION_NAME" 2>/dev/null; then
            echo -e "${YELLOW}Bot is already running in tmux session '$SESSION_NAME'${NC}"
            echo "  - Attach with: tmux attach -t $SESSION_NAME"
            echo "  - Stop with:   ./run_overnight.sh stop"
            exit 1
        fi

        echo -e "${GREEN}Starting MEV bot for overnight run...${NC}"
        echo "  Log file: $LOG_FILE"
        
        # Start in tmux with logging
        tmux new-session -d -s "$SESSION_NAME" \
            "cd /home/ubuntu/biggie && ./target/release/biggie 2>&1 | tee '$LOG_FILE'; echo 'Bot exited. Press any key to close.'; read"
        
        echo -e "${GREEN}Bot started successfully!${NC}"
        echo ""
        echo "Commands:"
        echo "  ./run_overnight.sh attach  - View live output (Ctrl+B, D to detach)"
        echo "  ./run_overnight.sh logs    - Tail the log file"
        echo "  ./run_overnight.sh status  - Check if running"
        echo "  ./run_overnight.sh stop    - Stop the bot"
        echo ""
        echo -e "${CYAN}You can safely disconnect from SSH now.${NC}"
        ;;
    
    attach)
        if tmux has-session -t "$SESSION_NAME" 2>/dev/null; then
            echo "Attaching to bot session. Press Ctrl+B, then D to detach safely."
            sleep 1
            tmux attach -t "$SESSION_NAME"
        else
            echo "Bot is not running. Start with: ./run_overnight.sh start"
        fi
        ;;
    
    logs)
        LATEST_LOG=$(ls -t "$LOG_DIR"/biggie_*.log 2>/dev/null | head -1)
        if [ -n "$LATEST_LOG" ]; then
            echo "Tailing: $LATEST_LOG"
            echo "Press Ctrl+C to stop watching"
            tail -f "$LATEST_LOG"
        else
            echo "No log files found in $LOG_DIR"
        fi
        ;;
    
    status)
        if tmux has-session -t "$SESSION_NAME" 2>/dev/null; then
            echo -e "${GREEN}Bot is RUNNING${NC} in tmux session '$SESSION_NAME'"
            
            # Show recent stats from log
            LATEST_LOG=$(ls -t "$LOG_DIR"/biggie_*.log 2>/dev/null | head -1)
            if [ -n "$LATEST_LOG" ]; then
                echo ""
                echo "Latest stats from log:"
                grep -E "Stats:|ARB|error" "$LATEST_LOG" | tail -10 || echo "  (no stats yet)"
                echo ""
                echo "Log file size: $(du -h "$LATEST_LOG" | cut -f1)"
            fi
        else
            echo "Bot is NOT running"
        fi
        ;;
    
    stop)
        if tmux has-session -t "$SESSION_NAME" 2>/dev/null; then
            echo "Stopping bot..."
            tmux send-keys -t "$SESSION_NAME" C-c
            sleep 2
            tmux kill-session -t "$SESSION_NAME" 2>/dev/null || true
            echo -e "${GREEN}Bot stopped${NC}"
        else
            echo "Bot is not running"
        fi
        ;;
    
    summary)
        # Show summary of all log files
        echo "=== Run History ==="
        for log in $(ls -t "$LOG_DIR"/biggie_*.log 2>/dev/null | head -5); do
            echo ""
            echo "--- $(basename "$log") ---"
            grep -E "Stats:|ARB FOUND|arbs_found" "$log" | tail -5 || echo "  (no stats)"
        done
        ;;
    
    *)
        echo "Usage: $0 {start|attach|logs|status|stop|summary}"
        exit 1
        ;;
esac
