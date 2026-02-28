export DYLD_LIBRARY_PATH="$(pwd)/src-tauri/bin/mac_aarch64/tor"
mkdir -p /tmp/test_tor_data_$$
./src-tauri/bin/mac_aarch64/tor/tor --SocksPort "9052 IsolateSOCKSAuth" --DataDirectory /tmp/test_tor_data_$$ >/dev/null 2>&1 &
TOR_PID=$!

echo "Tor daemon starting with PID $TOR_PID..."
# Wait for bootstrap
for i in {1..20}; do
    sleep 3
    if curl --max-time 10 -s -x socks5h://127.0.0.1:9052 https://check.torproject.org/ >/dev/null; then
        echo "Tor bootstrapped and proxy active!"
        break
    fi
done

echo "----------------------------------------"
echo "Attempting to probe LockBit target URL..."
echo "----------------------------------------"
curl -I -v -x socks5h://127.0.0.1:9052 "http://lockbit6vhrjaqzsdj6pqalyideigxv4xycfeyunpx35znogiwmojnid.onion/secret/5ebb49ccc01e4337b258f53deab3588e-6faad228-bbfb-33ff-be8b-c86f7e5ed518/terracaribbean.com/terracaribbean.com.7z"

kill $TOR_PID
rm -rf /tmp/test_tor_data_$$
