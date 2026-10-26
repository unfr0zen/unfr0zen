/* Makes the EM3581's nHOST_INT line (GPIO2_IO04, gpio 36) a wake source, so a
 * Zigbee callback wakes the unit from suspend to RAM. Unloading undoes it. */
#include <linux/gpio.h>
#include <linux/gpio/consumer.h>
#include <linux/interrupt.h>
#include <linux/module.h>
#include <linux/device.h>

#define NHOST_INT_GPIO 36

static int irq;
static struct wakeup_source *wake;

static irqreturn_t zbwake_irq(int number, void *data)
{
	/* SPI responses also assert nHOST_INT. Let the host finish the transfer
	 * before suspend can freeze it with chip select still asserted. */
	__pm_wakeup_event(wake, 100);
	return IRQ_HANDLED;
}

static int __init zbwake_init(void)
{
	struct gpio_desc *desc = gpio_to_desc(NHOST_INT_GPIO);
	int ret;

	if (!desc)
		return -ENODEV;
	irq = gpiod_to_irq(desc);
	if (irq < 0)
		return irq;
	wake = wakeup_source_register("em3581");
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

static void __exit zbwake_exit(void)
{
	irq_set_irq_wake(irq, 0);
	free_irq(irq, &irq);
	wakeup_source_unregister(wake);
	pr_info("zbwake: nHOST_INT no longer wakes the unit\n");
}

module_init(zbwake_init);
module_exit(zbwake_exit);
MODULE_LICENSE("GPL");
MODULE_VERSION("2");
MODULE_DESCRIPTION("EM3581 nHOST_INT wake source");
