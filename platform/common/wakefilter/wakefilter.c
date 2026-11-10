/* Preloaded into nlsleepd (the gen 2's and the gen 3's alike), which sets the Wi-Fi chip's wake patterns before
 * each sleep (nl80211 SET_WOWLAN) and can only describe one kind: a frame to
 * an address and a port. This adds the patterns nestlocal lists in
 * /var/run/unfr0zen/wake-patterns to the ones nlsleepd sends, one a line:
 *
 *     OFFSET MASK PATTERN
 *
 * the packet offset in decimal, then nl80211's mask (a bit a pattern byte,
 * least significant first) and the pattern bytes in hex. The chip takes
 * MAX_PATTERNS: nlsleepd's come first and the list fills what is left.
 *
 * Whatever nlsleepd's init script runs on the way to it is preloaded with
 * this too, and has no libnl: its functions are weak references, used only
 * where nl_send_auto_complete is called, and once in a process with libnl
 * the preload is taken out of the environment its children get.
 *
 * Built without libc headers: what it calls is declared here. */

typedef unsigned char u8;
typedef unsigned short u16;
typedef unsigned int u32;

struct nl_sock;
struct nl_msg;
struct nlmsghdr { u32 len; u16 type; u16 flags; u32 seq; u32 pid; };
struct genlmsghdr { u8 cmd; u8 version; u16 reserved; };
struct nlattr { u16 len; u16 type; };

#define WEAK __attribute__((weak))
extern struct nlmsghdr *nlmsg_hdr(struct nl_msg *) WEAK;
extern struct nl_msg *nlmsg_alloc(void) WEAK;
extern void nlmsg_free(struct nl_msg *) WEAK;
extern void *genlmsg_put(struct nl_msg *, u32 pid, u32 seq, int family, int hdrlen, int flags, u8 cmd, u8 version) WEAK;
extern int nla_put(struct nl_msg *, int type, int len, const void *data) WEAK;
extern struct nlattr *nla_nest_start(struct nl_msg *, int type) WEAK;
extern int nla_nest_end(struct nl_msg *, struct nlattr *) WEAK;
extern void *dlsym(void *, const char *);
extern int open(const char *, int, ...);
extern int read(int, void *, unsigned int);
extern int close(int);
extern int unsetenv(const char *);

#define RTLD_NEXT ((void *)-1)
#define NL80211_CMD_SET_WOWLAN 74
#define NL80211_ATTR_WOWLAN_TRIGGERS 117
#define NL80211_WOWLAN_TRIG_PKT_PATTERN 4
#define NL80211_PKTPAT_MASK 1
#define NL80211_PKTPAT_PATTERN 2
#define NL80211_PKTPAT_OFFSET 3
#define NLA_TYPE_MASK 0x3fff
#define MAX_PATTERNS 8
#define MAX_PATTERN_LEN 128
#define PATTERNS "/var/run/unfr0zen/wake-patterns"

#define ALIGNED(n) (((n) + 3u) & ~3u)
#define FOR_EACH_ATTR(a, start, end) \
	for (a = (struct nlattr *)(start); (u8 *)a + 4 <= (u8 *)(end) && a->len >= 4 && (u8 *)a + a->len <= (u8 *)(end); \
	     a = (struct nlattr *)((u8 *)a + ALIGNED(a->len)))

static int hex(const char **p, u8 *out, int max)
{
	int n = 0;

	for (;; n++) {
		int value = 0, digits;

		for (digits = 0; digits < 2; digits++, (*p)++) {
			char c = **p;

			if (c >= '0' && c <= '9')
				value = value * 16 + c - '0';
			else if (c >= 'a' && c <= 'f')
				value = value * 16 + c - 'a' + 10;
			else
				break;
		}
		if (digits == 0)
			return n;
		if (digits == 1 || n == max)
			return -1;
		out[n] = value;
	}
}

/* Adds the list's patterns to the nest being written, numbered after the
 * count already in it. A line that does not parse ends the list. */
static void add_listed(struct nl_msg *msg, int count)
{
	static char text[4096];
	const char *p = text;
	int fd = open(PATTERNS, 0), length;

	if (fd < 0)
		return;
	length = read(fd, text, sizeof(text) - 1);
	close(fd);
	if (length <= 0)
		return;
	text[length] = 0;
	while (*p && count < MAX_PATTERNS) {
		u8 mask[MAX_PATTERN_LEN / 8], pattern[MAX_PATTERN_LEN];
		u32 offset = 0;
		int mask_len, pattern_len;
		struct nlattr *entry;

		if (*p < '0' || *p > '9')
			return;
		while (*p >= '0' && *p <= '9')
			offset = offset * 10 + *p++ - '0';
		if (*p++ != ' ')
			return;
		mask_len = hex(&p, mask, sizeof(mask));
		if (mask_len < 1 || *p++ != ' ')
			return;
		pattern_len = hex(&p, pattern, sizeof(pattern));
		if (pattern_len < 1 || mask_len != (pattern_len + 7) / 8 || *p++ != '\n')
			return;
		entry = nla_nest_start(msg, ++count);
		if (!entry)
			return;
		nla_put(msg, NL80211_PKTPAT_MASK, mask_len, mask);
		nla_put(msg, NL80211_PKTPAT_PATTERN, pattern_len, pattern);
		/* The gen 2's cfg80211 predates the attribute: without it the
		 * pattern is at the frame's start. */
		if (offset)
			nla_put(msg, NL80211_PKTPAT_OFFSET, sizeof(offset), &offset);
		nla_nest_end(msg, entry);
	}
}

/* The message with the list's patterns added to its triggers, or NULL when
 * it is not one that enables wake on WLAN. */
static struct nl_msg *with_listed(struct nlmsghdr *hdr)
{
	struct genlmsghdr *genl = (struct genlmsghdr *)(hdr + 1);
	u8 *end = (u8 *)hdr + hdr->len;
	struct nlattr *attr, *triggers = 0, *sub, *entry, *nest, *patterns;
	struct nl_msg *msg;
	int count = 0;

	if (hdr->len < sizeof(*hdr) + sizeof(*genl) || genl->cmd != NL80211_CMD_SET_WOWLAN)
		return 0;
	FOR_EACH_ATTR(attr, genl + 1, end)
		if ((attr->type & NLA_TYPE_MASK) == NL80211_ATTR_WOWLAN_TRIGGERS)
			triggers = attr;
	if (!triggers)
		return 0;
	msg = nlmsg_alloc();
	if (!msg)
		return 0;
	if (!genlmsg_put(msg, 0, 0, hdr->type, 0, hdr->flags, genl->cmd, genl->version))
		goto failed;
	FOR_EACH_ATTR(attr, genl + 1, end)
		if (attr != triggers && nla_put(msg, attr->type, attr->len - 4, attr + 1) < 0)
			goto failed;
	nest = nla_nest_start(msg, NL80211_ATTR_WOWLAN_TRIGGERS);
	if (!nest)
		goto failed;
	FOR_EACH_ATTR(sub, triggers + 1, (u8 *)triggers + triggers->len)
		if ((sub->type & NLA_TYPE_MASK) != NL80211_WOWLAN_TRIG_PKT_PATTERN &&
		    nla_put(msg, sub->type, sub->len - 4, sub + 1) < 0)
			goto failed;
	patterns = nla_nest_start(msg, NL80211_WOWLAN_TRIG_PKT_PATTERN);
	if (!patterns)
		goto failed;
	FOR_EACH_ATTR(sub, triggers + 1, (u8 *)triggers + triggers->len) {
		if ((sub->type & NLA_TYPE_MASK) != NL80211_WOWLAN_TRIG_PKT_PATTERN)
			continue;
		FOR_EACH_ATTR(entry, sub + 1, (u8 *)sub + sub->len)
			if (nla_put(msg, ++count, entry->len - 4, entry + 1) < 0)
				goto failed;
	}
	add_listed(msg, count);
	nla_nest_end(msg, patterns);
	nla_nest_end(msg, nest);
	return msg;
failed:
	nlmsg_free(msg);
	return 0;
}

__attribute__((constructor)) static void loaded(void)
{
	if (nlmsg_hdr)
		unsetenv("LD_PRELOAD");
}

int nl_send_auto_complete(struct nl_sock *sock, struct nl_msg *msg)
{
	static int (*send)(struct nl_sock *, struct nl_msg *);
	struct nl_msg *extended;
	int result;

	if (!send)
		send = dlsym(RTLD_NEXT, "nl_send_auto_complete");
	extended = with_listed(nlmsg_hdr(msg));
	if (!extended)
		return send(sock, msg);
	result = send(sock, extended);
	nlmsg_free(extended);
	return result;
}
