#!/bin/bash
pkill -f "p 20999" 2>/dev/null
sleep 1
echo "TCP conns to usque: $(ss -tnp 2>/dev/null | grep -c usque)"
echo "UDP conns to usque: $(ss -unp 2>/dev/null | grep -c usque)"
echo "--- sample TCP ---"
ss -tnp 2>/dev/null | grep usque | head -2
echo "--- sample UDP ---"
ss -unp 2>/dev/null | grep usque | head -2
