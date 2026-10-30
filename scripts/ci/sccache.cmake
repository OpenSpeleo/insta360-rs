# CI-only compiler caching. Linking and archiving keep their normal commands.
find_program(INSTA360_SCCACHE sccache REQUIRED)
# Ninja's compile-rule launcher covers ASM and Metal Objective-C++ as well as
# C/C++; CMake's C/CXX compiler launchers alone would miss those languages.
set_property(GLOBAL PROPERTY RULE_LAUNCH_COMPILE "\"${INSTA360_SCCACHE}\"")
if(CMAKE_HOST_SYSTEM_NAME STREQUAL "Windows")
    find_program(INSTA360_PYTHON python REQUIRED)
    set_property(GLOBAL PROPERTY RULE_LAUNCH_COMPILE
        "\"${INSTA360_PYTHON}\" \"${CMAKE_CURRENT_LIST_DIR}/sccache-msvc.py\" \"${INSTA360_SCCACHE}\"")
endif()
