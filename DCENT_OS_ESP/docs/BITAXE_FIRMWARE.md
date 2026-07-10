# DCENT_OS for BitAxe — AI-Native Mining Firmware

## Vision
D-Central edition of AxeOS (ESP-Miner) with a **built-in MCP server — an
AI-control surface ESP-Miner/AxeOS do not provide** that makes every Bitaxe
an AI-controllable mining device.

Primary differentiator: **Model Context Protocol (MCP) built into the firmware**.
Any MCP-compatible AI (Claude, OpenClaw, custom agents) can inspect, control,
and optimize the miner through natural language.

Core features:
- MCP server on the ESP32 — AI assistants control your miner
- OpenClaw skill + Claude Code skill for plug-and-play AI integration
- All existing AxeOS features preserved (web UI, REST API, OTA, swarm)
- Hex multi-chip management, swarm coordination
- D-Central pool as default, space heater integration
- Zero dev fee, fully open source (GPL-3.0)

## Base: ESP-Miner / AxeOS
- **URL**: https://github.com/bitaxeorg/ESP-Miner
- **Language**: C 58.2%, SCSS 18.3%, TypeScript 13.4%, HTML 6.5%
- **License**: GPL-3.0
- **Platform**: ESP32-S3 (FreeRTOS)

### Source Structure
```
ESP-Miner/
  components/asic/          # ASIC chip drivers
    bm1366.c               # BM1366 driver (BitaxeUltra)
    bm1368.c               # BM1368 driver (BitaxeSupra)
    bm1370.c               # BM1370 driver (BitaxeGamma/GT)
    bm1397.c               # BM1397 driver (BitaxeMax)
    include/mining.h        # Common mining structures
  main/                     # Primary firmware logic
    tasks/                  # FreeRTOS tasks (power mgmt, etc.)
  http_server/              # Web UI and REST API
    openapi.yaml            # API specification
```

### Current AxeOS Features
- Browser-based control panel
- Core voltage and frequency adjustment
- Real-time monitoring (hashrate, ASIC temp, VRM temp, efficiency)
- WiFi and pool configuration
- OTA firmware updates (web or USB-C)
- REST API for automation
- Swarm dashboard (v2.4.1+)

## D-Central Enhancements

### Hex-Specific Features
- Multi-chip management (6x BM1366 or BM1368)
- Per-chip tuning interface
- Aggregate hashrate display
- Power distribution monitoring across chips
- Thermal balancing between chips

### Swarm Coordination
- Automatic peer discovery on local network
- Centralized configuration push
- Coordinated pool switching
- Fleet-wide OTA updates
- Total fleet hashrate dashboard

### Space Heater Integration
- Temperature targeting (set room temp, auto-adjust hashrate)
- Schedule profiles (heat mode, quiet mode, off)
- BTU output display
- Home Assistant auto-discovery (MQTT)
- Power consumption tracking with heat output correlation

### D-Central Pool Integration
- D-Central pool pre-configured
- Solo/pool switch with explicit confirmation
- Pool stats in firmware dashboard
- Share count and estimated earnings

## MCP Server — The Killer Feature

### Why MCP on a Miner?
MCP (Model Context Protocol) is Anthropic's open standard for AI-tool integration.
By embedding an MCP server directly in the Bitaxe firmware, we make the miner a
first-class citizen in the AI ecosystem. Any AI assistant can:

- **Monitor**: "What's my Bitaxe hashrate and temperature?"
- **Control**: "Set my Bitaxe to 400 MHz and reduce fan to 50%"
- **Optimize**: "Find the most efficient frequency for my Bitaxe"
- **Automate**: "Mine at full power when electricity is cheap, throttle when expensive"
- **Diagnose**: "Why is my Bitaxe running hot? Check the ASIC temp trend"
- **Fleet**: "Show me the status of all my Bitaxes on the network"

### MCP Architecture on ESP32
```
ESP32-S3 (FreeRTOS)
├── Mining Task (existing ESP-Miner)
│   ├── ASIC driver (BM1366/1368/1370/1397)
│   ├── Stratum client
│   └── Power management
├── HTTP Server (existing)
│   ├── Web UI (existing AxeOS dashboard)
│   ├── REST API (existing /api/*)
│   └── MCP endpoint (/mcp)          ← NEW
│       ├── JSON-RPC 2.0 handler
│       ├── Tool definitions (12+ tools)
│       └── Resource definitions
└── mDNS (existing, for discovery)
```

### MCP Tools (Bitaxe-specific)
| Tool | Description |
|------|-------------|
| get_status | Hashrate, temp, frequency, voltage, efficiency |
| get_asic_info | Chip model, core count, nonce found count |
| set_frequency | Adjust ASIC frequency (MHz) |
| set_core_voltage | Adjust core voltage (mV) |
| set_fan_speed | Fan PWM percentage (0-100%) |
| set_pool | Configure pool URL, worker, password |
| get_network | WiFi SSID, IP, signal strength |
| get_history | Hashrate/temp history (last N minutes) |
| restart_mining | Restart the mining task |
| ota_check | Check for firmware updates |
| get_swarm | List other Bitaxes on the network |
| run_autotune | Start frequency/voltage optimization |

### MCP Resources
| URI | Description |
|-----|-------------|
| bitaxe://status | Live status (subscribable) |
| bitaxe://history | Rolling hashrate/temp history |
| bitaxe://config | Current configuration |

### Implementation Notes
- ESP32 HTTP server already handles REST — add /mcp POST handler
- JSON parsing: cJSON (already in ESP-IDF)
- Memory: MCP handler ~20 KB RAM overhead (acceptable on ESP32-S3 with 512 KB SRAM)
- Reuse existing REST API internals — MCP tools call same functions
- Protocol version: 2025-03-26 (Streamable HTTP transport)

## AI Integration Skills

### OpenClaw Skill
OpenClaw is an open-source AI agent framework. A DCENT_OS skill allows:
```yaml
# openclaw-dcentos-skill
name: dcentos-bitaxe
description: Control and monitor Bitaxe miners running DCENT_OS
transport: streamable-http
endpoint: http://{miner_ip}/mcp
tools: [get_status, set_frequency, set_pool, ...]
```
Users install the skill, point it at their Bitaxe IP, and their AI can control mining.

### Claude Code Skill (MCP Server Config)
For Claude Desktop / Claude Code users:
```json
{
  "mcpServers": {
    "bitaxe": {
      "url": "http://203.0.113.100/mcp",
      "transport": "streamable-http"
    }
  }
}
```
Then: "Claude, what's my Bitaxe doing?" → Claude calls get_status → shows results.

### Home Assistant Integration
- MQTT auto-discovery (existing AxeOS pattern)
- HA sensors: hashrate, temperature, power, efficiency
- HA switches: mining on/off, fan speed, pool selection
- Works alongside MCP — HA for automation, MCP for AI conversation

## Development Approach
1. Fork ESP-Miner repository
2. **Add MCP server endpoint** (/mcp) — JSON-RPC 2.0 handler using cJSON
3. Implement MCP tool handlers wrapping existing REST API functions
4. Add Hex-specific multi-chip management
5. Implement swarm discovery protocol
6. Add Home Assistant MQTT integration
7. Build enhanced web UI with D-Central branding
8. Create OpenClaw skill package
9. Create Claude MCP server config template
10. Set up OTA update infrastructure
11. Test across all BitAxe models D-Central sells

## Compatibility
- BitAxe Ultra (BM1366) - Hex Ultra
- BitAxe Supra (BM1368) - Hex Supra
- BitAxe Gamma (BM1370) - GT
- BitAxe Max (BM1397) - legacy
- NerdMiner / NerdAxe variants

## Relationship to DCENT_OS for Antminers
The MCP protocol and tool definitions are **shared** between Bitaxe DCENT_OS and
Antminer DCENT_OS. Same AI skills work with both. The only difference is the
transport and available tools:

| Feature | Bitaxe (ESP32) | Antminer (Linux) |
|---------|---------------|-----------------|
| MCP Transport | HTTP on port 80 | HTTP on port 3000 |
| Language | C (ESP-IDF) | Python (stdlib) |
| Tools | 12 (single chip) | 12+ (multi-chain) |
| Resources | 3 | 3 |
| Protocol | MCP 2025-03-26 | MCP 2025-03-26 |

Same AI skill, different miners. "Control my mining fleet" works whether you have
1 Bitaxe or 100 Antminers — or a mix of both.
