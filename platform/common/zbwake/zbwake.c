/* Makes the radio's nHOST_INT line a wake source, so a
 * Zigbee callback wakes the unit from suspend to RAM. Unloading undoes it. */
#include <linux/gpio.h>
#include <linux/version.h>
#if LINUX_VERSION_CODE >= KERNEL_VERSION(3, 13, 0)
#include <linux/gpio/consumer.h>
#endif
#include <linux/interrupt.h>
#include <linux/module.h>
#include <linux/device.h>

#include "board.h"

static int irq;
static struct wakeup_source *wake;

static irqreturn_t zbwake_irq(int number, void *data) {
    /* SPI responses also assert nHOST_INT. Let the host finish the transfer
     * before suspend can freeze it with chip select still asserted. */
    __pm_wakeup_event(wake, 100);
    return IRQ_HANDLED;
}

static int __init zbwake_init(void) {
#if LINUX_VERSION_CODE >= KERNEL_VERSION(3, 13, 0)
    struct gpio_desc *desc = gpio_to_desc(NHOST_INT_GPIO);
#endif
    int ret;

#if LINUX_VERSION_CODE >= KERNEL_VERSION(3, 13, 0)
    if (!desc)
        return -ENODEV;
    irq = gpiod_to_irq(desc);
#else
    irq = gpio_to_irq(NHOST_INT_GPIO);
#endif
    if (irq < 0)
        return irq;
    wake = wakeup_source_register(WAKE_NAME);
    if (!wake)
        return -ENOMEM;
    ret = request_irq(irq, zbwake_irq, IRQF_SHARED | IRQF_TRIGGER_FALLING, "zbwake", &irq);
    if (ret) {
        wakeup_source_unregister(wake);
        return ret;
    }
    ret = irq_set_irq_wake(irq, 1);
    if (ret) {
        free_irq(irq, &irq);
        wakeup_source_unregister(wake);
        return ret;
    }
    pr_info("zbwake: nHOST_INT (gpio %d, irq %d) wakes the unit\n", NHOST_INT_GPIO, irq);
    return 0;
}

static void __exit zbwake_exit(void) {
    irq_set_irq_wake(irq, 0);
    free_irq(irq, &irq);
    wakeup_source_unregister(wake);
    pr_info("zbwake: nHOST_INT no longer wakes the unit\n");
}

module_init(zbwake_init);
module_exit(zbwake_exit);
MODULE_LICENSE("GPL");
MODULE_VERSION(ZBWAKE_VERSION);
MODULE_DESCRIPTION("802.15.4 nHOST_INT wake source");
