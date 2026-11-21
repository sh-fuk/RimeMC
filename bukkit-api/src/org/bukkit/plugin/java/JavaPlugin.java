package org.bukkit.plugin.java;

import java.io.*;
import java.net.URL;
import java.net.URLClassLoader;
import java.nio.file.Files;
import java.util.logging.Logger;

import org.bukkit.Server;
import org.bukkit.command.Command;
import org.bukkit.command.CommandSender;
import org.bukkit.command.PluginCommand;
import org.bukkit.configuration.file.FileConfiguration;
import org.bukkit.configuration.file.YamlConfiguration;
import org.bukkit.plugin.Plugin;
import org.bukkit.plugin.PluginDescriptionFile;

public abstract class JavaPlugin implements Plugin {
    private PluginDescriptionFile description;
    private File dataFolder;
    private File file;
    private ClassLoader loader;
    private boolean enabled;
    private Logger logger;
    private FileConfiguration config;

    public JavaPlugin() {}

    /** Called by the RimeMC loader. */
    public final void init(PluginDescriptionFile d, File data, File jar, ClassLoader cl) {
        description = d; dataFolder = data; file = jar; loader = cl;
        logger = dev.rime.bukkit.RimeLoggers.forPlugin(d.getName());
    }

    @Override public final String getName() { return description.getName(); }
    @Override public final PluginDescriptionFile getDescription() { return description; }
    @Override public final File getDataFolder() { return dataFolder; }
    protected final File getFile() { return file; }
    protected final ClassLoader getClassLoader() { return loader; }
    @Override public final Logger getLogger() { return logger; }
    @Override public final Server getServer() { return org.bukkit.Bukkit.getServer(); }
    @Override public final boolean isEnabled() { return enabled; }

    public final void setEnabled(boolean e) {
        if (enabled == e) return;
        enabled = e;
        if (e) onEnable(); else onDisable();
    }
    public final void forceDisabled() { enabled = false; }

    @Override public void onLoad() {}
    @Override public void onEnable() {}
    @Override public void onDisable() {}
    @Override public boolean onCommand(CommandSender sender, Command command, String label, String[] args) { return false; }
    @Override public java.util.List<String> onTabComplete(CommandSender sender, Command command, String label, String[] args) { return null; }

    public PluginCommand getCommand(String name) {
        PluginCommand c = dev.rime.bukkit.Bridge.pluginCommand(name);
        return c != null && c.getPlugin() == this ? c : null;
    }

    @Override public InputStream getResource(String filename) {
        URL u = loader instanceof URLClassLoader ? ((URLClassLoader) loader).findResource(filename) : loader.getResource(filename);
        try { return u == null ? null : u.openStream(); } catch (IOException e) { return null; }
    }

    public void saveResource(String path, boolean replace) {
        File out = new File(dataFolder, path);
        if (out.exists() && !replace) return;
        try (InputStream in = getResource(path)) {
            if (in == null) throw new IllegalArgumentException("The embedded resource '" + path + "' cannot be found in " + file);
            out.getParentFile().mkdirs();
            Files.copy(in, out.toPath(), java.nio.file.StandardCopyOption.REPLACE_EXISTING);
        } catch (IOException e) { logger.severe("Could not save " + out.getName() + ": " + e); }
    }

    @Override public FileConfiguration getConfig() { if (config == null) reloadConfig(); return config; }
    @Override public void reloadConfig() {
        YamlConfiguration c = YamlConfiguration.loadConfiguration(new File(dataFolder, "config.yml"));
        try (InputStream in = getResource("config.yml")) {
            if (in != null) c.setDefaults(YamlConfiguration.loadConfiguration(new InputStreamReader(in, java.nio.charset.StandardCharsets.UTF_8)));
        } catch (IOException ignored) {}
        config = c;
    }
    @Override public void saveConfig() {
        try { getConfig().save(new File(dataFolder, "config.yml")); } catch (IOException e) { logger.severe("Could not save config: " + e); }
    }
    @Override public void saveDefaultConfig() { if (!new File(dataFolder, "config.yml").exists()) saveResource("config.yml", false); }

    @Override public String toString() { return description.getFullName(); }
}
