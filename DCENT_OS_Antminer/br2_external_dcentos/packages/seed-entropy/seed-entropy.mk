################################################################################
#
# seed-entropy
#
################################################################################

SEED_ENTROPY_VERSION = 2.0.0
SEED_ENTROPY_SITE = $(BR2_EXTERNAL_DCENTOS_PATH)/packages/seed-entropy/src
SEED_ENTROPY_SITE_METHOD = local
SEED_ENTROPY_LICENSE = GPL-3.0+
SEED_ENTROPY_LICENSE_FILES = seed-entropy.c

define SEED_ENTROPY_BUILD_CMDS
	$(TARGET_CC) $(TARGET_CFLAGS) $(TARGET_LDFLAGS) \
		-o $(@D)/seed-entropy $(@D)/seed-entropy.c
endef

define SEED_ENTROPY_INSTALL_TARGET_CMDS
	install -D -m 0755 $(@D)/seed-entropy $(TARGET_DIR)/usr/sbin/seed-entropy
endef

$(eval $(generic-package))
