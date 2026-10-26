/* zbwake for the gen2 (j49, Linux 2.6.37): holds the unit awake briefly on
 * each falling edge of the EM357's nHOST_INT (gpio 182), so that suspend
 * cannot freeze an SPI transfer with chip select asserted. Nest's own
 * diamond-zigbee driver already makes the line a wake source at suspend.
 * Built against the j49's kernel by platform/j49/kernel.sh. Unloading undoes it. */
#include <linux/gpio.h>
#include <linux/interrupt.h>
#include <linux/module.h>
#include <linux/pm_wakeup.h>

#define NHOST_INT_GPIO 182

static int irq;
static struct wakeup_source *wake;

static irqreturn_t zbwake_irq(int number, void *data)
{
	__pm_wakeup_event(wake, 100);
	return IRQ_HANDLED;
}

static int __init zbwake_init(void)
{
	int ret;

	irq = gpio_to_irq(NHOST_INT_GPIO);
	if (irq < 0)
		return irq;
	wake = wakeup_source_register("em357");
	if (!wake)
		return -ENOMEM;
	ret = request_irq(irq, zbwake_irq, IRQF_SHARED | IRQF_TRIGGER_FALLING, "zbwake", &irq);
	if (ret) {
		wakeup_source_unregister(wake);
		return ret;
	}
	ret = set_irq_wake(irq, 1);
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
	set_irq_wake(irq, 0);
	free_irq(irq, &irq);
	wakeup_source_unregister(wake);
	pr_info("zbwake: nHOST_INT no longer wakes the unit\n");
}

module_init(zbwake_init);
module_exit(zbwake_exit);
MODULE_LICENSE("GPL");
MODULE_VERSION("2");
MODULE_DESCRIPTION("EM357 nHOST_INT wake source");
