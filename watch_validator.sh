#!/bin/bash
echo "🔍 Watching validator status... (Ctrl+C to stop)"
echo ""

while true; do
    # Check for key status indicators in recent logs
    STATUS=$(sudo tail -200 /home/solana/logs/validator.log 2>/dev/null | grep -E "health|caught up|replay complete|waiting for|slot [0-9]+ root|optimistically confirmed|starting services|rpc service" -i | tail -1)
    
    # Check if generating index
    INDEX=$(sudo tail -50 /home/solana/logs/validator.log 2>/dev/null | grep "generating index" | tail -1)
    
    # Check if replaying
    REPLAY=$(sudo tail -50 /home/solana/logs/validator.log 2>/dev/null | grep -E "replayed [0-9]+ slots|replay stage" -i | tail -1)
    
    # Check for RPC ready
    RPC_READY=$(sudo tail -500 /home/solana/logs/validator.log 2>/dev/null | grep -E "rpc.*listening|JSON RPC|rpc service" -i | tail -1)
    
    # Check health via RPC
    HEALTH=$(curl -s -X POST http://localhost:8899 -H "Content-Type: application/json" -d '{"jsonrpc":"2.0","id":1,"method":"getHealth"}' 2>/dev/null)
    
    TIMESTAMP=$(date '+%H:%M:%S')
    
    if echo "$HEALTH" | grep -q '"result":"ok"'; then
        echo ""
        echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
        echo "✅ [$TIMESTAMP] VALIDATOR IS READY AND HEALTHY!"
        echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
        
        # Get slot info
        SLOT_INFO=$(curl -s -X POST http://localhost:8899 -H "Content-Type: application/json" -d '{"jsonrpc":"2.0","id":1,"method":"getSlot"}' 2>/dev/null)
        echo "Current slot: $(echo $SLOT_INFO | grep -oP '"result":\K[0-9]+')"
        
        # Play notification sound or send alert
        echo -e "\a"  # Terminal bell
        
        # Optional: desktop notification if available
        which notify-send >/dev/null 2>&1 && notify-send "Solana Validator" "Validator is ready and healthy!" 2>/dev/null
        
        break
    elif [ -n "$INDEX" ]; then
        PROGRESS=$(echo "$INDEX" | grep -oP 'processed \K[0-9]+/[0-9]+')
        echo -ne "\r⏳ [$TIMESTAMP] Building index: $PROGRESS slots...          "
    elif [ -n "$REPLAY" ]; then
        echo -ne "\r🔄 [$TIMESTAMP] Replaying blocks...                         "
    elif [ -n "$RPC_READY" ]; then
        echo -ne "\r🌐 [$TIMESTAMP] RPC starting up...                          "
    else
        echo -ne "\r⏳ [$TIMESTAMP] Initializing...                             "
    fi
    
    sleep 5
done

echo ""
echo "You can now use the validator RPC at http://localhost:8899"
