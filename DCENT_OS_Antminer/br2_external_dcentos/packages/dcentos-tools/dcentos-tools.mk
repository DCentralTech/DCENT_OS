################################################################################
#
# dcentos-tools
#
################################################################################

DCENTOS_TOOLS_VERSION = 0.1.0
DCENTOS_TOOLS_SITE = $(BR2_EXTERNAL_DCENTOS_PATH)/board/zynq/rootfs-overlay/root/tools
DCENTOS_TOOLS_SITE_METHOD = local
DCENTOS_TOOLS_LICENSE = GPL-3.0+

define DCENTOS_TOOLS_INSTALL_TARGET_CMDS
	mkdir -p $(TARGET_DIR)/root/tools
	cp -a $(@D)/*.py $(TARGET_DIR)/root/tools/
	cp -a $(@D)/*.sh $(TARGET_DIR)/root/tools/ 2>/dev/null || true
	chmod +x $(TARGET_DIR)/root/tools/*.py
	chmod +x $(TARGET_DIR)/root/tools/*.sh 2>/dev/null || true

	# Install dcent-shell launcher
	if [ -f $(BR2_EXTERNAL_DCENTOS_PATH)/board/zynq/rootfs-overlay/usr/bin/dcent-shell ]; then \
		install -D -m 0755 $(BR2_EXTERNAL_DCENTOS_PATH)/board/zynq/rootfs-overlay/usr/bin/dcent-shell \
			$(TARGET_DIR)/usr/bin/dcent-shell; \
	fi

	# Install web dashboard and MCP server
	mkdir -p $(TARGET_DIR)/root/web/static
	install -m 0755 $(BR2_EXTERNAL_DCENTOS_PATH)/board/zynq/rootfs-overlay/root/web/server.py \
		$(TARGET_DIR)/root/web/server.py
	install -m 0755 $(BR2_EXTERNAL_DCENTOS_PATH)/board/zynq/rootfs-overlay/root/web/mcp_server.py \
		$(TARGET_DIR)/root/web/mcp_server.py
	cp -a $(BR2_EXTERNAL_DCENTOS_PATH)/board/zynq/rootfs-overlay/root/web/static/* \
		$(TARGET_DIR)/root/web/static/
endef

$(eval $(generic-package))
