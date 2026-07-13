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
	# Runtime package contains API adapters only. Standalone hardware research
	# executors remain source artifacts and are deliberately absent from normal
	# images because they bypass dcentrald's exclusive hardware owner.
	mkdir -p $(TARGET_DIR)/root/web/static
	install -m 0755 $(BR2_EXTERNAL_DCENTOS_PATH)/board/zynq/rootfs-overlay/root/web/server.py \
		$(TARGET_DIR)/root/web/server.py
	install -m 0755 $(BR2_EXTERNAL_DCENTOS_PATH)/board/zynq/rootfs-overlay/root/web/mcp_server.py \
		$(TARGET_DIR)/root/web/mcp_server.py
	cp -a $(BR2_EXTERNAL_DCENTOS_PATH)/board/zynq/rootfs-overlay/root/web/static/* \
		$(TARGET_DIR)/root/web/static/
endef

$(eval $(generic-package))
