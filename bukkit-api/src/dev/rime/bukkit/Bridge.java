package dev.rime.bukkit;

import java.io.File;
import java.lang.reflect.Method;
import java.net.URL;
import java.net.URLClassLoader;
import java.util.*;
import java.util.jar.JarFile;
import java.util.zip.ZipEntry;

import org.bukkit.Bukkit;
import org.bukkit.block.Block;
import org.bukkit.command.Command;
import org.bukkit.command.PluginCommand;
import org.bukkit.entity.Player;
import org.bukkit.event.Cancellable;
import org.bukkit.event.Event;
import org.bukkit.event.EventHandler;
import org.bukkit.event.EventPriority;
import org.bukkit.event.Listener;
import org.bukkit.event.block.BlockBreakEvent;
import org.bukkit.event.block.BlockPlaceEvent;
import org.bukkit.event.player.AsyncPlayerChatEvent;
import org.bukkit.event.player.PlayerCommandPreprocessEvent;
import org.bukkit.event.player.PlayerJoinEvent;
import org.bukkit.event.player.PlayerQuitEvent;
import org.bukkit.plugin.Plugin;
import org.bukkit.plugin.PluginDescriptionFile;
import org.bukkit.plugin.java.JavaPlugin;

/** The RimeMC side of the plugin host: natives implemented in Rust and entry points Rust calls. */
public final class Bridge {
    private Bridge() {}

    // ---------------------------------------------------------------- natives (Rust)
    public static native void log(int level, String msg);
    public static native void broadcast(String msg);
    public static native String[] players();
    public static native String playerUuid(String name);
    public static native double[] playerPos(String name);
    public static native void sendMessage(String name, String msg);
    public static native void request(String name, int kind, String arg, double x, double y, double z, float yaw, float pitch);
    public static native boolean isOp(String name);
    public static native int gamemode(String name);
    public static native double health(String name);
    public static native String getBlock(int x, int y, int z);
    public static native boolean setBlock(int x, int y, int z, String material);
    public static native boolean giveItem(String name, String material, int count);
    public static native long worldTime();
    public static native void setWorldTime(long t);
    public static native int maxPlayers();
    public static native String motd();
    public static native String dispatchConsole(String line);

    // ---------------------------------------------------------------- state
    static final List<JavaPlugin> plugins = new ArrayList<>();
    static final Map<String, PluginCommand> commands = new HashMap<>();
    static final Map<String, Boolean> permissionDefaults = new HashMap<>();
    private static final List<Handler> handlers = new ArrayList<>();
    private static final Map<String, RimePlayer> playerObjects = new HashMap<>();

    static final class Handler {
        final Plugin plugin; final Listener listener; final Method method; final EventPriority priority; final boolean ignoreCancelled;
        Handler(Plugin p, Listener l, Method m, EventPriority pr, boolean ic) { plugin = p; listener = l; method = m; priority = pr; ignoreCancelled = ic; }
    }

    public static synchronized Player player(String name) {
        if (name == null) return null;
        String uuid = playerUuid(name);
        if (uuid == null) { playerObjects.remove(name.toLowerCase(Locale.ROOT)); return null; }
        return playerObjects.computeIfAbsent(name.toLowerCase(Locale.ROOT), k -> new RimePlayer(name, UUID.fromString(uuid)));
    }

    public static void registerEvents(Listener l, Plugin p) {
        for (Method m : l.getClass().getMethods()) {
            EventHandler eh = m.getAnnotation(EventHandler.class);
            if (eh == null || m.getParameterCount() != 1 || !Event.class.isAssignableFrom(m.getParameterTypes()[0])) continue;
            m.setAccessible(true);
            synchronized (handlers) {
                handlers.add(new Handler(p, l, m, eh.priority(), eh.ignoreCancelled()));
                handlers.sort(Comparator.comparingInt(h -> h.priority.getSlot()));
            }
        }
    }

    public static void unregister(Plugin p) {
        synchronized (handlers) { handlers.removeIf(h -> h.plugin == p); }
    }

    public static void callEvent(Event e) {
        List<Handler> hs;
        synchronized (handlers) { hs = new ArrayList<>(handlers); }
        for (Handler h : hs) {
            if (!h.method.getParameterTypes()[0].isInstance(e) || !h.plugin.isEnabled()) continue;
            if (h.ignoreCancelled && e instanceof Cancellable && ((Cancellable) e).isCancelled()) continue;
            try {
                h.method.invoke(h.listener, e);
            } catch (Throwable t) {
                Throwable c = t instanceof java.lang.reflect.InvocationTargetException ? t.getCause() : t;
                log(2, "Could not pass event " + e.getEventName() + " to " + h.plugin.getName() + ": " + c);
            }
        }
    }

    // ---------------------------------------------------------------- loading
    /** Loads every plugin jar in {@code dir}; returns the loaded plugin names. */
    public static String[] load(String dir) {
        File[] jars = new File(dir).listFiles((d, n) -> n.endsWith(".jar"));
        if (jars == null) return new String[0];
        Arrays.sort(jars);
        List<String> names = new ArrayList<>();
        for (File jar : jars) {
            try {
                PluginDescriptionFile desc;
                try (JarFile jf = new JarFile(jar)) {
                    ZipEntry e = jf.getEntry("plugin.yml");
                    if (e == null) { log(1, jar.getName() + " has no plugin.yml, skipped"); continue; }
                    desc = new PluginDescriptionFile(jf.getInputStream(e));
                }
                URLClassLoader cl = new URLClassLoader(new URL[] { jar.toURI().toURL() }, Bridge.class.getClassLoader());
                Class<?> main = Class.forName(desc.getMain(), true, cl);
                JavaPlugin p = (JavaPlugin) main.getDeclaredConstructor().newInstance();
                p.init(desc, new File(dir, desc.getName()), jar, cl);
                for (Map.Entry<String, Map<String, Object>> c : desc.getCommandMap().entrySet()) {
                    PluginCommand pc = new PluginCommand(c.getKey(), p, c.getValue());
                    commands.put(c.getKey().toLowerCase(Locale.ROOT), pc);
                    for (String a : pc.getAliases()) commands.putIfAbsent(a.toLowerCase(Locale.ROOT), pc);
                }
                for (Map.Entry<String, Object> perm : desc.getPermissionDefaults().entrySet())
                    permissionDefaults.put(perm.getKey(), "true".equals(String.valueOf(perm.getValue())));
                try { p.onLoad(); } catch (Throwable t) { log(2, desc.getName() + " onLoad failed: " + t); }
                plugins.add(p);
                names.add(desc.getFullName());
            } catch (Throwable t) {
                log(2, "Could not load " + jar.getName() + ": " + t);
            }
        }
        return names.toArray(new String[0]);
    }

    public static void enable() {
        for (JavaPlugin p : plugins) {
            log(0, "Enabling " + p.getDescription().getFullName());
            try { p.setEnabled(true); } catch (Throwable t) { log(2, "Error enabling " + p.getName() + ": " + t); p.forceDisabled(); }
        }
    }

    public static void disable() {
        for (int i = plugins.size() - 1; i >= 0; i--) {
            JavaPlugin p = plugins.get(i);
            if (!p.isEnabled()) continue;
            log(0, "Disabling " + p.getDescription().getFullName());
            try { p.setEnabled(false); } catch (Throwable t) { log(2, "Error disabling " + p.getName() + ": " + t); }
            unregister(p);
            RimeScheduler.INSTANCE.cancelTasks(p);
        }
    }

    public static void tick() { RimeScheduler.INSTANCE.tick(); }

    // ---------------------------------------------------------------- events from Rust
    /** @return the join message to broadcast ("" = none) */
    public static String join(String name, String message) {
        Player p = player(name);
        if (p == null) return message;
        PlayerJoinEvent e = new PlayerJoinEvent(p, message);
        callEvent(e);
        return e.getJoinMessage() == null ? "" : e.getJoinMessage();
    }

    public static String quit(String name, String message) {
        Player p = player(name);
        if (p == null) return message;
        PlayerQuitEvent e = new PlayerQuitEvent(p, message);
        callEvent(e);
        synchronized (Bridge.class) { playerObjects.remove(name.toLowerCase(Locale.ROOT)); }
        return e.getQuitMessage() == null ? "" : e.getQuitMessage();
    }

    /** @return the formatted chat line, or null when a plugin cancelled it */
    public static String chat(String name, String message) {
        Player p = player(name);
        if (p == null) return "<" + name + "> " + message;
        Set<Player> recipients = new HashSet<>(Bukkit.getOnlinePlayers());
        AsyncPlayerChatEvent e = new AsyncPlayerChatEvent(false, p, message, recipients);
        callEvent(e);
        if (e.isCancelled()) return null;
        try { return String.format(e.getFormat(), p.getDisplayName(), e.getMessage()); }
        catch (IllegalFormatException ex) { return "<" + p.getDisplayName() + "> " + e.getMessage(); }
    }

    /** @return true when a plugin handled (or cancelled) the command */
    public static boolean command(String name, String line) {
        Player p = player(name);
        if (p == null) return false;
        PlayerCommandPreprocessEvent pre = new PlayerCommandPreprocessEvent(p, "/" + line);
        callEvent(pre);
        if (pre.isCancelled()) return true;
        String msg = pre.getMessage().startsWith("/") ? pre.getMessage().substring(1) : pre.getMessage();
        String[] parts = msg.trim().split("\\s+");
        if (parts.length == 0 || parts[0].isEmpty()) return false;
        String label = parts[0];
        int colon = label.indexOf(':');
        PluginCommand c = commands.get((colon >= 0 ? label.substring(colon + 1) : label).toLowerCase(Locale.ROOT));
        if (c == null) return false;
        c.execute(p, label, Arrays.copyOfRange(parts, 1, parts.length));
        return true;
    }

    /** @return true when cancelled */
    public static boolean blockBreak(String name, int x, int y, int z) {
        Player p = player(name);
        if (p == null) return false;
        Block b = p.getWorld().getBlockAt(x, y, z);
        BlockBreakEvent e = new BlockBreakEvent(b, p);
        callEvent(e);
        return e.isCancelled();
    }

    public static boolean blockPlace(String name, int x, int y, int z, String material) {
        Player p = player(name);
        if (p == null) return false;
        Block b = p.getWorld().getBlockAt(x, y, z);
        BlockPlaceEvent e = new BlockPlaceEvent(b, p, org.bukkit.Material.matchMaterial(material));
        callEvent(e);
        return e.isCancelled();
    }

    public static PluginCommand pluginCommand(String name) { return name == null ? null : commands.get(name.toLowerCase(Locale.ROOT)); }

    public static String[] commandNames() { return commands.keySet().toArray(new String[0]); }
}
