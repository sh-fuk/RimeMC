package org.bukkit.plugin;

import java.io.File;
import java.io.InputStream;
import java.util.logging.Logger;

import org.bukkit.Server;
import org.bukkit.command.TabExecutor;
import org.bukkit.configuration.file.FileConfiguration;

public interface Plugin extends TabExecutor {
    String getName();
    PluginDescriptionFile getDescription();
    File getDataFolder();
    Logger getLogger();
    Server getServer();
    boolean isEnabled();
    void onLoad();
    void onEnable();
    void onDisable();
    FileConfiguration getConfig();
    void saveConfig();
    void saveDefaultConfig();
    void reloadConfig();
    InputStream getResource(String filename);
}
